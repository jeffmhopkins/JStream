//! The built `jstream-daemon` binary on its own backends, streaming to the real receiver
//! and recording at the same time: no fakes and no in-process wiring. It runs as a user
//! would run it, with its data, config and log folders in the test's scratch folder, and
//! a client drives it over its WebSocket API: a colour source on the canvas, an RTMP
//! output with a key and a recording, `output.start` on both.
//!
//! On Linux it has no session bus, so the stream key goes to ADR-0071's `0600` file, not
//! the developer's keyring. On macOS the key goes to the Keychain, as the product keeps
//! it, but into a login keychain the test makes in its scratch home
//! ([`login_keychain`]), under an output ID no real collection uses, and the test clears
//! it at the end. VideoToolbox always has an H.264 encoder, so the daemon must stream
//! there.
//!
//! What comes out on Linux depends on the machine's H.264 encoders, which the daemon
//! chooses as the product does:
//! - **With one** (VA-API, or Cisco's OpenH264 installed in the daemon's store): the
//!   output goes `active`, and the receiver gets H.264 at the canvas size and AAC, with
//!   red pixels.
//! - **Without one**: the start fails, and the reason tells the user how to get one
//!   (OpenH264, or a VA-API driver). This is a machine with neither, running the harness
//!   on the libx264 stand-in.
//!
//! When `JSTREAM_E2E_OPENH264` is set (the xtask's `--openh264-download`, which CI runs,
//! or `--openh264 <library>`), the test puts Cisco's library in the daemon's store, where
//! the daemon checks it against the pin, and then requires the stream.

use std::io::{BufRead, BufReader};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, Event, Hello, OutputId, OutputState, PROTOCOL_VERSION,
    ProtocolError, SecretString,
};
use jstream_control::{Control, ControlError, WsConfig, WsControl};
#[cfg(target_os = "linux")]
use jstream_platform_linux::OpenH264Store;
use serde_json::{Value, json};

#[cfg(target_os = "linux")]
use crate::openh264_library;
use crate::probe::{Receiver, free_port, pixel, probe};
use crate::{CANVAS, KEY, near, scratch};

/// The stream's output ID. On macOS its key is a real Keychain item, so the ID is one
/// no one's collection has: the test can't overwrite a real output's key.
const LIVE: &str = "jstream-receiver-e2e-live";
/// The recording's output ID.
const REC: &str = "jstream-receiver-e2e-rec";

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
#[cfg(target_os = "linux")]
fn data_dir(home: &Path) -> PathBuf {
    home.join("data").join("jstream")
}

/// Starts the binary with everything it writes under `dir`, and waits for its
/// `listening on` line. Its stderr (the log) goes to the test's.
fn start_daemon(dir: &Path) -> Daemon {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("the home folder");
    #[cfg(target_os = "macos")]
    login_keychain(&home);
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

/// Gives the scratch `home` a login keychain of its own: unlocked, never relocking, and
/// the only one its user searches. macOS keeps the keychain list under `HOME`, so the
/// daemon, which runs with this `HOME`, stores its stream key here, never in the
/// developer's keychain. Without one, the Security framework would ask, in a dialog no
/// one answers on a runner, where to keep it.
///
/// Before changing anything it checks that `security` reads the list from this `HOME`:
/// a fresh home lists no keychain. If it lists one, the list is the developer's own, so
/// the test stops rather than change it.
#[cfg(target_os = "macos")]
fn login_keychain(home: &Path) {
    let security = |args: &[&std::ffi::OsStr]| {
        let out = Command::new("security")
            .args(args)
            .env("HOME", home)
            .stdin(Stdio::null())
            .output()
            .expect("`security` runs");
        assert!(
            out.status.success(),
            "security {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let os = std::ffi::OsStr::new;
    let listed = security(&[os("list-keychains"), os("-d"), os("user")]);
    assert!(
        listed.trim().is_empty(),
        "`security` with HOME={} lists keychains already ({}): it isn't reading this \
         HOME, so the test won't change that list",
        home.display(),
        listed.trim()
    );
    let keychains = home.join("Library/Keychains");
    std::fs::create_dir_all(&keychains).expect("the keychain folder");
    let path = keychains.join("login.keychain-db");
    let kc = path.as_os_str();
    security(&[os("create-keychain"), os("-p"), os("e2e"), kc]);
    security(&[os("set-keychain-settings"), kc]);
    security(&[os("unlock-keychain"), os("-p"), os("e2e"), kc]);
    security(&[os("list-keychains"), os("-d"), os("user"), os("-s"), kc]);
    security(&[os("default-keychain"), os("-d"), os("user"), os("-s"), kc]);
}

/// Puts Cisco's library from `JSTREAM_E2E_OPENH264` ([`openh264_library`]) in the
/// daemon's store, which checks it against the pin when it loads. Whether it did.
#[cfg(target_os = "linux")]
fn install_openh264(home: &Path) -> bool {
    let Some(library) = openh264_library() else {
        return false;
    };
    let store = OpenH264Store::new(data_dir(home).join("codecs").join("openh264"));
    let path = store.library_path();
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("the store's folder");
    std::fs::copy(&library, &path).expect("the library is stored");
    true
}

/// On macOS there is nothing to install: VideoToolbox's encoder is part of the OS, so
/// the daemon has one.
#[cfg(target_os = "macos")]
fn install_openh264(_home: &Path) -> bool {
    true
}

async fn call(control: &WsControl, call: Call) -> Result<CallResult, ControlError> {
    let what = format!("{call:?}");
    tokio::time::timeout(Duration::from_secs(20), control.call(call))
        .await
        .unwrap_or_else(|_| panic!("the daemon answers within 20 s: {what}"))
        .map(|response| response.result)
}

async fn edit(control: &WsControl, edits: Value) {
    let batch: Batch = serde_json::from_value(json!({ "edits": edits })).expect("a valid batch");
    call(control, Call::Batch(batch))
        .await
        .unwrap_or_else(|e| panic!("the batch was refused: {e}"));
}

/// The real daemon, on its own backends, streams and records a colour, or on Linux says
/// how to get the H.264 encoder it lacks.
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter: cargo xtask e2e-stream"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_binary_streams_and_records_on_its_own_backends() {
    let dir = scratch("binary");
    // Whether the daemon has an H.264 encoder, so must stream.
    let encoder = install_openh264(&dir.join("home"));
    let recording = dir.join("binary.mp4");
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
            {"type": "output.create", "output_id": LIVE,
             "kind": {"type": "rtmp", "target": {"type": "generic",
                      "url": format!("rtmp://127.0.0.1:{port}/live")}}},
            {"type": "output.create", "output_id": REC,
             "kind": {"type": "record", "path": recording.to_str().expect("a UTF-8 path"),
                      "container": "mp4"}},
        ]),
    )
    .await;
    call(
        &control,
        Call::Action {
            action: Action::OutputSetKey {
                output_id: OutputId::new(LIVE),
                key: SecretString::new(KEY),
            },
        },
    )
    .await
    .expect("the key is stored");
    // ADR-0071's fallback: no session bus, so the key is in the config folder's file.
    #[cfg(target_os = "linux")]
    {
        let secrets = dir.join("home/config/jstream/secrets");
        let text = std::fs::read_to_string(&secrets).expect("the fallback file holds the key");
        assert!(text.contains(KEY), "the key is in {}", secrets.display());
    }
    let result = stream_and_record(&control, &dir, port, &recording, encoder).await;
    // The Keychain item outlives the daemon: take it out whether or not the run passed.
    call(
        &control,
        Call::Action {
            action: Action::OutputClearKey {
                output_id: OutputId::new(LIVE),
            },
        },
    )
    .await
    .expect("output.clear_key");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Starts the stream and the recording, and checks what each one wrote. `Err` holds a
/// failed check's panic, so the caller can clean up before it fails the test.
async fn stream_and_record(
    control: &WsControl,
    dir: &Path,
    port: u16,
    recording: &Path,
    encoder: bool,
) -> std::thread::Result<()> {
    use futures_util::FutureExt;
    std::panic::AssertUnwindSafe(stream_and_record_checked(
        control, dir, port, recording, encoder,
    ))
    .catch_unwind()
    .await
}

async fn stream_and_record_checked(
    control: &WsControl,
    dir: &Path,
    port: u16,
    recording: &Path,
    encoder: bool,
) {
    let mut events = control.events();
    let receiver = Receiver::start(port, KEY, &dir.join("binary.flv"));
    let started = call(control, start(LIVE)).await;
    // The start is accepted, and then the pipeline opens its encoders: either it goes
    // active, or it fails and says why.
    let failed = started.as_ref().err().map(|e| match e.protocol() {
        Some(ProtocolError::ActionFailed { reason }) => reason.clone(),
        _ => format!("{e}"),
    });
    if let Some(reason) = failed {
        drop(receiver.kill());
        eprintln!("the daemon has no H.264 encoder here: {reason}");
        assert!(
            !encoder,
            "the daemon has an H.264 encoder (VideoToolbox, or OpenH264 in its store), so it \
             must stream: {reason}"
        );
        assert!(
            reason.contains("OpenH264") && reason.contains("VA-API"),
            "the reason says how to get an H.264 encoder: {reason}"
        );
        return;
    }
    call(control, start(REC))
        .await
        .expect("the recording starts");
    let mut active = (false, false);
    let deadline = Instant::now() + Duration::from_secs(30);
    while active != (true, true) {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = tokio::time::timeout(left, events.next())
            .await
            .expect("the stream and the recording go active within 30 s")
            .expect("the event stream stays open");
        if let Event::OutputStateChanged { output_id, state } = event {
            match state {
                OutputState::Active if output_id.as_str() == LIVE => active.0 = true,
                OutputState::Active if output_id.as_str() == REC => active.1 = true,
                // The start was accepted and failed later: no reason comes with the
                // event, so say where to look rather than blame the encoder.
                OutputState::Failed => panic!(
                    "{output_id} failed after `output.start` was accepted; the reason is in \
                     the daemon's log, {}",
                    dir.join("logs").display()
                ),
                _ => {}
            }
        }
    }

    // Five seconds on the air, then stop both and read what arrived.
    tokio::time::sleep(Duration::from_secs(5)).await;
    for id in [LIVE, REC] {
        call(
            control,
            Call::Action {
                action: Action::OutputStop {
                    output_id: OutputId::new(id),
                },
            },
        )
        .await
        .expect("output.stop");
    }
    let file = receiver.finish();
    let got = probe(&file);
    eprintln!("the daemon binary's stream: {got:?}");
    check(&file, &got);

    // The recorder finishes its file and renames it from `.part` when it stops.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !recording.is_file() {
        assert!(
            Instant::now() < deadline,
            "the recording is at {} within 20 s of `output.stop`",
            recording.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let got = probe(recording);
    eprintln!("the daemon binary's recording: {got:?}");
    check(recording, &got);
}

fn start(id: &str) -> Call {
    Call::Action {
        action: Action::OutputStart {
            output_id: OutputId::new(id),
        },
    }
}

/// What the stream and the recording must both hold: H.264 at the canvas size, AAC, more
/// than three of the five seconds, and red pixels.
fn check(file: &Path, got: &crate::probe::Probe) {
    assert_eq!(got.video_codec, "h264", "{got:?}");
    assert_eq!(
        (got.width, got.height),
        (u64::from(CANVAS.0), u64::from(CANVAS.1)),
        "{got:?}"
    );
    assert_eq!(got.audio_codec.as_deref(), Some("aac"), "{got:?}");
    assert!(got.duration > 3.0, "{got:?}");
    let rgb = pixel(file, 1.0, u64::from(CANVAS.0 / 2), u64::from(CANVAS.1 / 2));
    assert!(near(rgb, [255, 0, 0]), "red, got {rgb:?}");
}
