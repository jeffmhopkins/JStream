//! The window on Linux (WebKitGTK) end to end: the built `jstream` opens its window on a
//! display, WebKitGTK loads the UI bundle from the app's `jstream` scheme, runs its WASM,
//! finds the bootstrap global and connects to the in-process engine with the token, and
//! the X11 global hotkey listener starts. It reads all of that from the app's log file.
//!
//! Ignored by default: it needs a display (`DISPLAY`, such as Xvfb's), the UI bundle
//! (`JSTREAM_UI_DIR`, `trunk build`'s `dist`) and a build with the `webkitgtk` feature.
//! CI's `desktop-linux` job runs it:
//! `cargo test -p jstream-desktop --features webkitgtk --test webkitgtk -- --ignored`.
//! With `JSTREAM_SCREENSHOT=<file>` it also saves the screen there (ImageMagick's
//! `import`), for the job's artifact.

#![cfg(all(target_os = "linux", feature = "webkitgtk"))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long the window may take to load the UI and connect: WebKitGTK compiles the
/// WASM on a CI runner's CPU.
const CONNECT: Duration = Duration::from_secs(90);

/// Kills the app when the test ends, however it ends.
struct App(Child);

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn log_text(data: &Path) -> String {
    std::fs::read_to_string(data.join("logs").join("jstream-desktop.log")).unwrap_or_default()
}

#[test]
#[ignore = "needs a display, the UI bundle (JSTREAM_UI_DIR) and the webkitgtk feature"]
fn the_window_loads_the_ui_and_connects_to_its_engine() {
    let ui = PathBuf::from(std::env::var_os("JSTREAM_UI_DIR").expect("JSTREAM_UI_DIR"));
    assert!(
        ui.join("index.html").is_file(),
        "no UI bundle in {}",
        ui.display()
    );
    assert!(std::env::var_os("DISPLAY").is_some(), "no DISPLAY");
    let home = std::env::temp_dir().join(format!("jstream-webkitgtk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("a home folder");
    let data = home.join("data").join("jstream");
    let app = App(Command::new(env!("CARGO_BIN_EXE_jstream"))
        .arg("--ui-dir")
        .arg(&ui)
        .env("HOME", &home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        // An X11 session, so the global hotkeys' listener starts on the display.
        .env("XDG_SESSION_TYPE", "x11")
        .env_remove("WAYLAND_DISPLAY")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the app starts"));
    let deadline = Instant::now() + CONNECT;
    let mut log = String::new();
    while Instant::now() < deadline {
        log = log_text(&data);
        if log.contains("handshake done") {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if let Some(file) = std::env::var_os("JSTREAM_SCREENSHOT") {
        // Give the page a moment to draw what the engine sent it.
        std::thread::sleep(Duration::from_secs(3));
        let shot = Command::new("import")
            .args(["-window", "root"])
            .arg(&file)
            .status();
        assert!(
            matches!(shot, Ok(status) if status.success()),
            "could not save a screenshot: {shot:?}"
        );
    }
    drop(app);
    assert!(
        log.contains("engine serving the webview"),
        "the engine didn't start:\n{log}"
    );
    assert!(
        log.contains("handshake done"),
        "the window's UI didn't connect to the engine within {CONNECT:?}:\n{log}"
    );
    assert!(
        log.contains("global hotkeys can be heard"),
        "the X11 global hotkeys didn't start:\n{log}"
    );
    assert!(!log.contains("the app could not start"), "{log}");
    let _ = std::fs::remove_dir_all(&home);
}
