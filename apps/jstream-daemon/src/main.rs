//! `jstream-daemon`: the headless engine host. It runs a `jstream-core` `Engine` and
//! serves it through `jstream-server`'s authenticated WebSocket API on `127.0.0.1`, one
//! `InProcessControl` session per connection, so `jstream-cli --url …` and
//! `jstream-cli mcp` can drive it, and MCP over streamable HTTP on the same port, one
//! session per MCP session (ADR-0001: the app only wires the engine to its
//! interfaces). It creates or reuses the user-only control token file (ADR-0006), keeps
//! the paired devices' token digests in a user-only file beside it (ADR-0044), logs
//! to stderr and to rotating files, writes local crash reports on a panic (R-SEC-004),
//! and stops gracefully on Ctrl-C. On Windows the engine is live: it runs the media
//! pipeline on the Windows backends and publishes over RTMP(S), after checking that
//! Media Foundation is installed, and `--register-vcam` registers the virtual camera's
//! media source (`register`). With `--collection` it loads the collection from a file
//! and saves every change there, and it records every batch, undo, redo and action in an
//! audit log next to its log files. Not yet: the web UI.
//!
//! Docs: docs/ARCHITECTURE.md#shape, docs/ARCHITECTURE.md#crate-map, apps/jstream-daemon/README.md#purpose, apps/jstream-daemon/README.md#usage, apps/jstream-daemon/README.md#design, docs/adr/0001-library-first-engine.md, docs/adr/0006-mcp-full-access.md

#![forbid(unsafe_code)]

mod args;
// Windows runs it at startup; its decision is tested everywhere.
#[cfg(any(windows, test))]
mod components;
mod crash;
mod logs;
mod register;
mod serve;
mod token;

use anyhow::Context;
use args::{Invocation, Options};
use jstream_core::AuditFile;
use jstream_protocol::PROTOCOL_VERSION;
use std::io::{Write, stderr, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Stopped by a signal, or printed the help or version.
const OK: u8 = 0;
/// Could not start (token file, port, runtime) or the server failed while serving.
const FAILED: u8 = 1;
/// The command line could not be parsed, or names no token file.
const USAGE: u8 = 2;
/// Windows lacks a component the engine needs: Media Foundation, on an N edition
/// without the Media Feature Pack or a Windows Server without its feature.
#[cfg(windows)]
const MISSING_COMPONENT: u8 = 3;

/// The environment variable naming the token file when `--token-file` is not given,
/// the same one `jstream-cli` reads.
const TOKEN_FILE_ENV: &str = "JSTREAM_TOKEN_FILE";

/// The environment variable that filters the log when `--log-level` is not given.
const LOG_ENV: &str = "RUST_LOG";

/// Why the daemon did not run.
enum Failure {
    Usage(String),
    Failed(anyhow::Error),
    /// The message says what is missing and how to install it.
    #[cfg(windows)]
    MissingComponent(String),
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure::Failed(error)
    }
}

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                // Best effort: the exit code reports the usage error either way.
                let _ = writeln!(
                    stderr().lock(),
                    "error: argument `{}` is not valid UTF-8",
                    arg.to_string_lossy()
                );
                return ExitCode::from(USAGE);
            }
        }
    }
    let result = match args::parse(&args) {
        Ok(Invocation::Help) => print(args::USAGE),
        Ok(Invocation::Version) => print(&format!(
            "jstream-daemon {}, protocol {PROTOCOL_VERSION}\n",
            env!("CARGO_PKG_VERSION")
        )),
        Ok(Invocation::RegisterVcam) => register::register()
            .map_err(Failure::Failed)
            .and_then(|message| print(&message)),
        Ok(Invocation::Serve(options)) => run(options),
        Err(message) => Err(Failure::Usage(format!(
            "{message}\n(run `jstream-daemon --help` for usage)"
        ))),
    };
    let code = match result {
        Ok(()) => OK,
        Err(Failure::Usage(message)) => {
            let _ = writeln!(stderr().lock(), "error: {message}");
            USAGE
        }
        Err(Failure::Failed(error)) => {
            // `{:#}` prints the whole chain of causes on one line.
            let _ = writeln!(stderr().lock(), "error: {error:#}");
            FAILED
        }
        #[cfg(windows)]
        Err(Failure::MissingComponent(message)) => {
            let _ = writeln!(stderr().lock(), "error: {message}");
            MISSING_COMPONENT
        }
    };
    ExitCode::from(code)
}

/// Writes `text` to stdout. Clippy denies `print!` (R-RS-005).
fn print(text: &str) -> Result<(), Failure> {
    let mut out = stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .context("could not write to stdout")?;
    Ok(())
}

/// The log filter: `--log-level`, else `RUST_LOG`, as `target=level` directives and an
/// optional bare default level, such as `jstream_server=debug,warn`; unset or empty means
/// `info`. A value that doesn't parse is a usage error.
fn log_filter(options: &Options) -> Result<Targets, Failure> {
    let (source, spec) = match &options.log_level {
        Some(spec) => ("--log-level", spec.clone()),
        None => (LOG_ENV, std::env::var(LOG_ENV).unwrap_or_default()),
    };
    if spec.trim().is_empty() {
        return Ok(Targets::new().with_default(LevelFilter::INFO));
    }
    spec.parse::<Targets>()
        .map_err(|error| Failure::Usage(format!("{source}: {error}")))
}

/// The log directory: `--log-dir`, else `logs` in the platform data directory.
fn log_dir(options: &Options) -> Result<PathBuf, Failure> {
    match &options.log_dir {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => logs::default_dir().ok_or_else(|| {
            Failure::Failed(anyhow::anyhow!(
                "could not find the data directory for the log files (no usable \
                 {}); pass `--log-dir <dir>`",
                if cfg!(windows) { "APPDATA" } else { "HOME" }
            ))
        }),
    }
}

/// Creates the log directory, installs the crash-report hook there, and installs the
/// process's log subscriber: the libraries' `tracing` spans and events, filtered by
/// `filter`, as text lines on stderr (stdout carries only the banner lines) and in the
/// rotating log files in `dir`.
fn init_logging(dir: &Path, filter: Targets) -> Result<(), Failure> {
    logs::create_dir(dir)
        .with_context(|| format!("could not create the log directory `{}`", dir.display()))?;
    crash::install(dir.to_owned());
    let files = logs::LogFiles::open(dir)
        .with_context(|| format!("could not create a log file in `{}`", dir.display()))?;
    let file_path = files.current_path();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(stderr))
        .with(tracing_subscriber::fmt::layer().with_writer(Arc::new(files)))
        .with(filter)
        .try_init()
        .context("could not install the log subscriber")?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_file = %file_path.display(),
        "jstream-daemon starting"
    );
    Ok(())
}

/// Checks that Windows has Media Foundation and its AAC encoder, before anything else
/// calls into it. The binary delay-loads the Media Foundation DLLs (`build.rs`), so on
/// an N edition without them the daemon still starts and gets here, and exits with
/// advice instead of a loader error.
#[cfg(windows)]
fn check_media_foundation() -> Result<(), Failure> {
    use jstream_platform::Codecs;
    let missing = |error: jstream_platform::PlatformError| {
        tracing::error!(%error, "a Windows component is missing");
        Failure::MissingComponent(error.to_string())
    };
    jstream_platform_windows::check_media_foundation().map_err(missing)?;
    let codecs = jstream_platform_windows::MfCodecs::new()
        .list()
        .context("could not list the Media Foundation codecs")?;
    components::aac_encoder(&codecs).map_err(missing)
}

/// Loads or creates the token, then serves until a signal arrives.
fn run(options: Options) -> Result<(), Failure> {
    let path = match options.token_file.clone() {
        Some(path) => PathBuf::from(path),
        None => match std::env::var_os(TOKEN_FILE_ENV) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => {
                return Err(Failure::Usage(format!(
                    "no control token file: pass `--token-file <path>` or set {TOKEN_FILE_ENV}"
                )));
            }
        },
    };
    let filter = log_filter(&options)?;
    let logs = log_dir(&options)?;
    init_logging(&logs, filter)?;
    #[cfg(windows)]
    check_media_foundation()?;
    #[cfg(windows)]
    register::log_status();
    // Next to the log files: every batch, undo, redo and action, without their
    // arguments (R-SEC-001).
    let audit = AuditFile::open(&logs)
        .with_context(|| format!("could not open the audit log in `{}`", logs.display()))?;
    // Checked before the token or collection file is written, so a mistyped
    // `--ui-dir` leaves nothing behind. `Server::bind` checks it again.
    if let Some(dir) = options.ui_dir.as_deref()
        && !Path::new(dir).is_dir()
    {
        return Err(anyhow::anyhow!("unusable UI directory `{dir}`: not a directory").into());
    }
    let (token, provenance) = token::load_or_create(&path)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;
    runtime.block_on(async {
        let stop = serve::shutdown_signal().context("could not install the Ctrl-C handler")?;
        let engine = serve::engine(&options, Arc::new(audit))?;
        let ui_dir = options.ui_dir.as_deref().map(Path::new);
        let device_file = token::device_file(&path);
        let server = serve::bind(engine, options.port, token, Some(device_file), ui_dir)
            .await
            .map_err(anyhow::Error::from)?;
        let shown = path.display();
        let token_line = match provenance {
            token::Provenance::Created => format!("token file: {shown} (created, new token)"),
            token::Provenance::Existing => format!("token file: {shown} (existing token)"),
        };
        // The URL line comes last: it tells a script or a test that the daemon serves.
        // Output is best effort: with stdout closed the server still runs.
        let mcp_line = server
            .mcp_url()
            .map(|url| format!("mcp endpoint: {url}\n"))
            .unwrap_or_default();
        let ui_line = server
            .ui_url()
            .map(|url| format!("ui: {url}\n"))
            .unwrap_or_default();
        let _ = print(&format!(
            "{token_line}\n{mcp_line}{ui_line}listening on {}\n",
            server.ws_url()
        ));
        server
            .serve_with_shutdown(stop)
            .await
            .map_err(anyhow::Error::from)?;
        let _ = print("stopped\n");
        Ok::<(), Failure>(())
    })
}
