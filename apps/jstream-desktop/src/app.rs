//! Startup: the command line, the log file, and starting the engine, its server and the
//! UI the window shows.
//!
//! Docs: apps/jstream-desktop/README.md#usage, apps/jstream-desktop/README.md#design

use crate::bootstrap;
use crate::host::{self, Host};
use crate::jtunnel;
use crate::logs::LogFile;
use crate::network;
use crate::ui::{self, Ui};
use anyhow::Context;
use jstream_crash::Secrets;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The UI bundle's folder beside the exe (in a macOS app, in its resources:
/// [`bundled_ui_dir`]), when `--ui-dir` doesn't name one.
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
    /// `--mcp`: let agents and other outside clients in, on a fixed port with the data
    /// folder's token file ([`host::Agents`]).
    pub mcp: bool,
    /// `--port <port>`: the port `--mcp` listens on, instead of [`host::MCP_PORT`].
    pub port: Option<u16>,
}

/// What the app takes, for the message about an argument it doesn't.
const USAGE: &str = "the app takes only `--ui-dir <folder>`, `--no-vcam`, `--mcp` and \
                     `--port <port>` (with `--mcp`)";

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
            Some("--mcp") => options.mcp = true,
            Some("--port") => match args.next().and_then(|port| port.to_str()?.parse().ok()) {
                Some(port) if port != 0 => options.port = Some(port),
                _ => return Err("`--port` needs a port from 1 to 65535".to_owned()),
            },
            _ => {
                return Err(format!(
                    "unknown argument `{}`; {USAGE}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    if options.port.is_some() && !options.mcp {
        return Err("`--port` is the port `--mcp` listens on: pass `--mcp` too".to_owned());
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
    /// The "Allow devices on my network" setting's file, which the window's
    /// `set_network` command writes, when the data folder is known.
    pub network_file: Option<PathBuf>,
    /// The "Receive from JTunnel senders" setting's file, which `set_jtunnel` writes,
    /// when the data folder is known.
    pub jtunnel_file: Option<PathBuf>,
}

impl Startup {
    /// The page saying the app couldn't start, for `message`.
    pub fn problem(message: String) -> Startup {
        Startup {
            ui: Ui::Problem(message),
            script: None,
            host: None,
            network_file: None,
            jtunnel_file: None,
        }
    }
}

/// Starts the engine and its server and finds the UI. Every failure becomes the page
/// the window shows, and is logged. The control token and every stream key are
/// registered with `secrets`, the crash reports' redaction.
pub fn start(
    options: &Options,
    exe_dir: Option<&Path>,
    data: Option<&Path>,
    secrets: &Secrets,
) -> Startup {
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
            secrets,
        )?;
        // After the backends, which `host::engine` sets: arrivals are stamped on the
        // media clock (ADR-0123 §1).
        jtunnel::start(&engine, &jtunnel::setting_file(data));
        let agents = options.mcp.then(|| host::Agents {
            port: options.port.unwrap_or(host::MCP_PORT),
            token_file: data.join(host::TOKEN_FILE),
        });
        let ui_settings = Some(data.join(jstream_platform::paths::UI_SETTINGS_FILE));
        let network = network_access(data);
        match Host::start(
            Arc::clone(&engine),
            ui::ORIGIN,
            secrets,
            ui_settings.clone(),
            agents.as_ref(),
            network.as_ref(),
        ) {
            Err(error) if network.is_some() => {
                // Most likely the network listener's port is in use (a daemon with
                // `--lan`): the app still starts, for this machine only. Any other cause
                // fails the retry the same way, with its own error.
                tracing::error!(
                    error = format!("{error:#}"),
                    "could not start with network access; trying again without it"
                );
                Host::start(
                    engine,
                    ui::ORIGIN,
                    secrets,
                    ui_settings,
                    agents.as_ref(),
                    None,
                )
            }
            started => started,
        }
    })();
    let vcam = host::vcam_status(exe_dir, !options.no_vcam);
    tracing::info!(vcam = vcam.as_str(), "virtual camera");
    let host = match host {
        Ok(host) => host,
        Err(error) => return problem(error),
    };
    let ui_dir = match (&options.ui_dir, exe_dir) {
        (Some(dir), _) => dir.clone(),
        (None, Some(exe_dir)) => bundled_ui_dir(exe_dir),
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
    let collection = data.to_string_lossy();
    let script = bootstrap::script(
        ui::ORIGIN,
        host.ws_url(),
        host.token(),
        &bootstrap::Page {
            vcam: vcam.as_str(),
            collection: collection.as_ref(),
            network: network::allowed(&network::setting_file(data)),
            network_open: host.network().is_some(),
            wheel_focus_only: wheel_focus_only() || !host.global_wheel(),
            jtunnel: &jtunnel::read(&jtunnel::setting_file(data)),
        },
    );
    Startup {
        ui,
        script: Some(script),
        host: Some(host),
        network_file: Some(network::setting_file(data)),
        jtunnel_file: Some(jtunnel::setting_file(data)),
    }
}

/// Whether macOS keeps wheel chords to the window with the focus: until the app has the
/// Accessibility permission (ADR-0144 §2), which it checks without asking. Elsewhere the
/// OS needs no permission for them. The page's `wheelFocusOnly` is this, or no listener
/// that hears the wheel ([`host::Host::global_wheel`]: none started, or Wayland's portal).
fn wheel_focus_only() -> bool {
    #[cfg(target_os = "macos")]
    {
        jstream_platform_macos::wheel_needs_accessibility()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// "Allow devices on my network", when the person turned it on: the network listener's
/// configuration, and the paired devices' file, both beside the data folder's token
/// file as the daemon keeps them. A certificate that can't be used is logged, and the
/// app starts without network access.
fn network_access(data: &Path) -> Option<host::Network> {
    if !network::allowed(&network::setting_file(data)) {
        return None;
    }
    let token_file = data.join(host::TOKEN_FILE);
    match network::config(&token_file) {
        Ok(config) => Some(host::Network {
            config,
            device_file: jstream_server::token_file::device_file(&token_file),
        }),
        Err(error) => {
            tracing::error!(
                error = format!("{error:#}"),
                "starting without network access"
            );
            None
        }
    }
}

/// The UI bundle for an exe in `exe_dir`: the [`UI_DIR`] folder beside it, or in a macOS
/// app (`JStream.app/Contents/MacOS/jstream`) the one in `Contents/Resources`, where an
/// app keeps what isn't code, so the bundle can be signed as it is.
pub fn bundled_ui_dir(exe_dir: &Path) -> PathBuf {
    let in_app = exe_dir.file_name().is_some_and(|name| name == "MacOS")
        && exe_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "Contents");
    match exe_dir.parent() {
        Some(contents) if in_app => contents.join("Resources").join(UI_DIR),
        _ => exe_dir.join(UI_DIR),
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
    fn a_macos_app_keeps_its_ui_in_resources() {
        use super::bundled_ui_dir;
        use std::path::Path;
        assert_eq!(
            bundled_ui_dir(Path::new("/Applications/JStream.app/Contents/MacOS")),
            Path::new("/Applications/JStream.app/Contents/Resources/ui")
        );
        assert_eq!(
            bundled_ui_dir(Path::new("/opt/jstream")),
            Path::new("/opt/jstream/ui")
        );
        // A `MacOS` folder outside an app's `Contents` is an ordinary folder.
        assert_eq!(
            bundled_ui_dir(Path::new("/tmp/MacOS")),
            Path::new("/tmp/MacOS/ui")
        );
    }

    #[test]
    fn the_command_line_takes_a_ui_folder_and_the_vcam_toggle() {
        assert_eq!(parse(&args(&[])), Ok(Options::default()));
        assert_eq!(
            parse(&args(&["--ui-dir", "dist"])),
            Ok(Options {
                ui_dir: Some(PathBuf::from("dist")),
                ..Options::default()
            })
        );
        assert_eq!(
            parse(&args(&["--no-vcam", "--ui-dir", "dist"])),
            Ok(Options {
                ui_dir: Some(PathBuf::from("dist")),
                no_vcam: true,
                ..Options::default()
            })
        );
        assert!(parse(&args(&["--ui-dir"])).is_err());
        assert!(parse(&args(&["--ui-dir", ""])).is_err());
        assert!(parse(&args(&["--port", "1"])).is_err());
    }

    /// `--mcp` lets agents in; `--port` moves it and means nothing without it.
    #[test]
    fn mcp_takes_an_optional_port() {
        assert_eq!(
            parse(&args(&["--mcp"])),
            Ok(Options {
                mcp: true,
                ..Options::default()
            })
        );
        assert_eq!(
            parse(&args(&["--mcp", "--port", "4460"])),
            Ok(Options {
                mcp: true,
                port: Some(4460),
                ..Options::default()
            })
        );
        let alone = parse(&args(&["--port", "4460"])).expect_err("needs --mcp");
        assert!(alone.contains("pass `--mcp` too"), "{alone}");
        for bad in [
            &["--mcp", "--port"][..],
            &["--mcp", "--port", "0"],
            &["--mcp", "--port", "x"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
