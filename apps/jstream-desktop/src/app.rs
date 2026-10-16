//! Startup: the command line, the log file, and starting the engine, its server and the
//! UI the window shows.
//!
//! Docs: apps/jstream-desktop/README.md#usage, apps/jstream-desktop/README.md#design

use crate::bootstrap;
use crate::host::{self, Host};
use crate::logs::LogFile;
use crate::ui::{self, Ui};
use anyhow::Context;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The UI bundle's folder beside the exe, when `--ui-dir` doesn't name one.
pub const UI_DIR: &str = "ui";

/// The collection file in the data folder.
pub const COLLECTION: &str = "collection.json";

/// The command line.
#[derive(Debug, Default, PartialEq)]
pub struct Options {
    /// `--ui-dir <dir>`: the UI bundle to serve instead of the `ui` folder beside the exe.
    pub ui_dir: Option<PathBuf>,
    /// `--no-vcam`: run the engine without the virtual camera.
    pub no_vcam: bool,
}

/// Parses the arguments after the program name.
pub fn parse(args: &[OsString]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--ui-dir") => match args.next() {
                Some(dir) if !dir.is_empty() => options.ui_dir = Some(PathBuf::from(dir)),
                _ => return Err("`--ui-dir` needs a folder".to_owned()),
            },
            Some("--no-vcam") => options.no_vcam = true,
            _ => {
                return Err(format!(
                    "unknown argument `{}`; the app takes only `--ui-dir <folder>` and `--no-vcam`",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    Ok(options)
}

/// What the window shows: the UI with the engine behind it, or a page saying why the
/// app couldn't start.
pub struct Startup {
    /// What the app's scheme serves.
    pub ui: Ui,
    /// The bootstrap script, when the engine started.
    pub script: Option<String>,
    /// The engine and its server, when they started.
    pub host: Option<Host>,
}

impl Startup {
    /// The page saying the app couldn't start, for `message`.
    pub fn problem(message: String) -> Startup {
        Startup {
            ui: Ui::Problem(message),
            script: None,
            host: None,
        }
    }
}

/// Starts the engine and its server and finds the UI. Every failure becomes the page
/// the window shows, and is logged.
pub fn start(options: &Options, exe_dir: Option<&Path>, data: Option<&Path>) -> Startup {
    let problem = |error: anyhow::Error| {
        tracing::error!(error = format!("{error:#}"), "the app could not start");
        Startup::problem(format!("{error:#}"))
    };
    let Some(data) = data else {
        return problem(anyhow::anyhow!(
            "could not find the data folder (no usable {})",
            if cfg!(windows) { "APPDATA" } else { "HOME" }
        ));
    };
    #[cfg(windows)]
    if let Err(error) = jstream_platform_windows::check_media_foundation() {
        return problem(anyhow::Error::from(error));
    }
    let host = (|| {
        let logs = data.join("logs");
        let audit = jstream_core::AuditFile::open(&logs)
            .with_context(|| format!("could not open the audit log in `{}`", logs.display()))?;
        let engine = host::engine(
            &data.join(COLLECTION),
            data,
            Arc::new(audit),
            !options.no_vcam,
        )?;
        Host::start(engine, ui::ORIGIN)
    })();
    let vcam = host::vcam_status(exe_dir, !options.no_vcam);
    tracing::info!(vcam = vcam.as_str(), "virtual camera");
    let host = match host {
        Ok(host) => host,
        Err(error) => return problem(error),
    };
    let ui_dir = match (&options.ui_dir, exe_dir) {
        (Some(dir), _) => dir.clone(),
        (None, Some(exe_dir)) => exe_dir.join(UI_DIR),
        (None, None) => PathBuf::from(UI_DIR),
    };
    let ui = Ui::files(&ui_dir, host.ws_origin());
    if let Ui::Problem(message) = ui {
        // Without a UI nothing can use the engine, so it stops, and the problem page
        // gets no bootstrap script.
        tracing::error!(%message, "no web UI");
        host.stop();
        return Startup::problem(message);
    }
    let script = bootstrap::script(ui::ORIGIN, host.ws_url(), host.token(), vcam.as_str());
    Startup {
        ui,
        script: Some(script),
        host: Some(host),
    }
}

/// Logs to the log file in `data`'s `logs` folder (and to stderr in a debug build),
/// filtered by `RUST_LOG` (default `info`). Returns the file's path, or why there is
/// none; the app runs either way.
pub fn init_logging(data: Option<&Path>) -> Result<PathBuf, String> {
    let filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|spec| !spec.trim().is_empty())
        .and_then(|spec| spec.parse::<Targets>().ok())
        .unwrap_or_else(|| Targets::new().with_default(LevelFilter::INFO));
    let file = data
        .ok_or_else(|| "no data folder".to_owned())
        .and_then(|data| LogFile::create(&data.join("logs")).map_err(|e| e.to_string()));
    let path = file
        .as_ref()
        .map(|file| file.path().to_owned())
        .map_err(Clone::clone);
    let file_layer = file.ok().map(|file| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(Arc::new(file))
    });
    let stderr_layer = cfg!(debug_assertions)
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .with(filter)
        .try_init();
    path
}

#[cfg(test)]
mod tests {
    use super::{Options, parse};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_command_line_takes_a_ui_folder_and_the_vcam_toggle() {
        assert_eq!(parse(&args(&[])), Ok(Options::default()));
        assert_eq!(
            parse(&args(&["--ui-dir", "dist"])),
            Ok(Options {
                ui_dir: Some(PathBuf::from("dist")),
                no_vcam: false,
            })
        );
        assert_eq!(
            parse(&args(&["--no-vcam", "--ui-dir", "dist"])),
            Ok(Options {
                ui_dir: Some(PathBuf::from("dist")),
                no_vcam: true,
            })
        );
        assert!(parse(&args(&["--ui-dir"])).is_err());
        assert!(parse(&args(&["--ui-dir", ""])).is_err());
        assert!(parse(&args(&["--port", "1"])).is_err());
    }
}
