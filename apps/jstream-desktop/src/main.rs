//! `jstream-desktop`: the JStream desktop app. A Tauri window shows the web UI
//! (`jstream-ui`), and the engine runs in the same process, served to the window over
//! the authenticated localhost WebSocket API. Windows first: the window needs WebView2,
//! so elsewhere the binary only says to use the daemon.
//!
//! Docs: apps/jstream-desktop/README.md#usage

#![forbid(unsafe_code)]
// A release build is a windowed app, with no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

/// Starts the engine and shows the window until it closes.
#[cfg(windows)]
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
    if let Some(data) = data.as_deref() {
        jstream_desktop::crash::install(data.join("logs"));
    }
    let log = app::init_logging(data.as_deref());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_file = ?log,
        data = ?data,
        "jstream-desktop starting"
    );
    let startup = match app::parse(&args) {
        Ok(options) => app::start(&options, exe_dir.as_deref(), data.as_deref()),
        Err(message) => Startup::problem(message),
    };
    match jstream_desktop::shell::run(startup.ui, startup.script, startup.host) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "the window failed");
            ExitCode::FAILURE
        }
    }
}

/// No window here yet: says so and fails.
#[cfg(not(windows))]
fn main() -> ExitCode {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "error: the desktop app runs on Windows only for now; run `jstream-daemon` and open \
         the web UI in a browser instead"
    );
    ExitCode::FAILURE
}
