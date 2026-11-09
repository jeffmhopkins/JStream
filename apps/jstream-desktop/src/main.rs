//! `jstream-desktop`: the JStream desktop app. A Tauri window shows the web UI
//! (`jstream-ui`), and the engine runs in the same process, served to the window over
//! the authenticated localhost WebSocket API. The window runs on Windows (WebView2) and on
//! Linux built with the `webkitgtk` feature (WebKitGTK); elsewhere the binary only says
//! to use the daemon.
//!
//! Docs: apps/jstream-desktop/README.md#usage

#![forbid(unsafe_code)]
// A release build is a windowed app, with no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

/// Starts the engine and shows the window until it closes.
#[cfg(webview)]
fn main() -> ExitCode {
    use jstream_desktop::app::{self, Startup};
    use std::ffi::OsString;
    use std::path::Path;

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_owned));
    let data =
        jstream_platform::paths::data_dir_for(exe_dir.as_deref(), |name| std::env::var_os(name));
    // Local crash reports beside the log file, as early as the folder is known
    // (R-SEC-004).
    // The hook, the token and the key store share one redaction list.
    let secrets = jstream_crash::Secrets::new();
    if let Some(data) = data.as_deref() {
        // On Windows each report gets a protected DACL for SYSTEM and the user, as the
        // reports are user-only on Unix (#656).
        #[cfg(windows)]
        let create: jstream_crash::CreateFile = jstream_platform_windows::create_private_file;
        #[cfg(not(windows))]
        let create: jstream_crash::CreateFile = jstream_crash::create_new;
        jstream_crash::install_creating(
            data.join("logs"),
            jstream_crash::Program {
                name: env!("CARGO_PKG_NAME"),
                version: env!("CARGO_PKG_VERSION"),
                handled: jstream_core::HANDLED_PANIC_THREADS,
            },
            secrets.clone(),
            crash_line,
            create,
        );
    }
    let log = app::init_logging(data.as_deref());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_file = ?log,
        data = ?data,
        "jstream-desktop starting"
    );
    let startup = match app::parse(&args) {
        Ok(options) => app::start(&options, exe_dir.as_deref(), data.as_deref(), &secrets),
        Err(message) => Startup::problem(message),
    };
    match jstream_desktop::shell::run(
        startup.ui,
        startup.script,
        startup.host,
        startup.network_file,
        startup.jtunnel_file,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "the window failed");
            ExitCode::FAILURE
        }
    }
}

/// Prints the crash hook's redacted line naming the report. Best effort: a release
/// build has no console, and the report is what matters.
#[cfg(webview)]
fn crash_line(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// No window here (not Windows, macOS, or Linux with `webkitgtk`): says so and fails.
#[cfg(not(webview))]
fn main() -> ExitCode {
    use std::io::Write;
    let message = if cfg!(target_os = "linux") {
        "error: this build of the desktop app has no window: build it with `--features \
         webkitgtk` (which needs WebKitGTK 4.1), or run `jstream-daemon` and open the web UI \
         in a browser"
    } else {
        "error: the desktop app has no window on this platform yet; run `jstream-daemon` and \
         open the web UI in a browser instead"
    };
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    ExitCode::FAILURE
}
