//! The command line: a small hand-rolled parser, like `jstream-cli`'s
//! (apps/jstream-daemon/README.md#design says why there is no parser crate).
//!
//! The parser is pure: the `JSTREAM_TOKEN_FILE` fallback is read by `main`, not here.

use jstream_protocol::OutputId;

/// The help text, printed by `--help` and `-h`.
pub const USAGE: &str = "\
jstream-daemon: run a headless JStream engine and serve its WebSocket API

Usage: jstream-daemon [options]

Options:
  --token-file <path>  the control token file (default: the JSTREAM_TOKEN_FILE
                       environment variable). Created, user-only, with a new
                       random token if it doesn't exist; reused if it does
  --port <port>        the port to listen on, on 127.0.0.1 (default: 0, any
                       free port; the URL is printed at startup)
  --output <id>        add a stopped placeholder output (repeatable)
  --log-dir <dir>      where log files and crash reports go (default: `logs`
                       in the platform data directory); created if missing
  --log-level <spec>   the log filter, as `target=level` directives and an
                       optional default level (default: RUST_LOG, else info)
  -V, --version        print the daemon and protocol versions
  -h, --help           print this help

It prints `listening on ws://127.0.0.1:<port>/api/v1/ws` once it serves, and runs
until Ctrl-C (or SIGTERM). Clients: jstream-cli --url <that URL> --token-file <path>.

Exit codes: 0 stopped by a signal, 1 failed to start or serve, 2 usage,
3 Media Foundation is missing (Windows N editions).
";

/// What the daemon serves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--token-file`. Without it `main` reads `JSTREAM_TOKEN_FILE`.
    pub token_file: Option<String>,
    /// `--port`: `0` lets the OS pick a free one.
    pub port: u16,
    /// `--output`: placeholder outputs, in the order given.
    pub outputs: Vec<OutputId>,
    /// `--log-dir`. Without it `main` uses `logs` in the platform data directory.
    pub log_dir: Option<String>,
    /// `--log-level`, unparsed: `main` parses it, and reads `RUST_LOG` without it.
    pub log_level: Option<String>,
}

/// A parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    /// Print the help text.
    Help,
    /// Print the versions.
    Version,
    /// Run the engine and serve it.
    Serve(Options),
}

/// Parses the arguments after the program name. The error is a usage message.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut options = Options::default();
    let mut port = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().ok_or_else(|| format!("`{arg}` needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            "--token-file" => {
                let path = value()?;
                if options.token_file.replace(path.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--port" => {
                let text = value()?;
                let parsed = text.parse::<u16>().map_err(|_| {
                    format!("`--port` must be a number from 0 to 65535, not `{text}`")
                })?;
                if port.replace(parsed).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--output" => options.outputs.push(OutputId::new(value()?.as_str())),
            "--log-dir" => {
                let dir = value()?;
                if options.log_dir.replace(dir.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--log-level" => {
                let spec = value()?;
                if options.log_level.replace(spec.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--collection" => {
                return Err(
                    "`--collection` is not supported yet: the engine has no persistence, \
                     so it always starts with an empty collection"
                        .to_owned(),
                );
            }
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    options.port = port.unwrap_or(0);
    Ok(Invocation::Serve(options))
}

#[cfg(test)]
mod tests {
    use super::{Invocation, Options, parse};
    use jstream_protocol::OutputId;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn serve(line: &str) -> Options {
        match parse(&args(line)) {
            Ok(Invocation::Serve(options)) => options,
            other => panic!("`{line}` should serve, got {other:?}"),
        }
    }

    fn error(line: &str) -> String {
        match parse(&args(line)) {
            Err(message) => message,
            other => panic!("`{line}` should be a usage error, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_serve_on_any_port_without_a_token_file() {
        assert_eq!(serve(""), Options::default());
        assert_eq!(serve("").port, 0);
    }

    #[test]
    fn options_are_read_in_any_order() {
        let options = serve(
            "--output out_1 --log-level debug --port 4455 --token-file /tmp/t \
             --log-dir /tmp/logs --output out_2",
        );
        assert_eq!(
            options,
            Options {
                token_file: Some("/tmp/t".to_owned()),
                port: 4455,
                outputs: vec![OutputId::new("out_1"), OutputId::new("out_2")],
                log_dir: Some("/tmp/logs".to_owned()),
                log_level: Some("debug".to_owned()),
            }
        );
    }

    #[test]
    fn help_and_version_win_over_the_rest() {
        assert_eq!(parse(&args("--port 1 --help")), Ok(Invocation::Help));
        assert_eq!(parse(&args("-h --bogus")), Ok(Invocation::Help));
        assert_eq!(parse(&args("--version")), Ok(Invocation::Version));
        assert_eq!(parse(&args("-V")), Ok(Invocation::Version));
    }

    #[test]
    fn bad_values_are_usage_errors() {
        assert_eq!(error("--port"), "`--port` needs a value");
        assert_eq!(error("--token-file"), "`--token-file` needs a value");
        assert_eq!(error("--output"), "`--output` needs a value");
        assert_eq!(error("--log-dir"), "`--log-dir` needs a value");
        assert_eq!(error("--log-level"), "`--log-level` needs a value");
        assert_eq!(error("--log-dir a --log-dir b"), "`--log-dir` given twice");
        assert_eq!(
            error("--log-level info --log-level warn"),
            "`--log-level` given twice"
        );
        assert_eq!(
            error("--port 65536"),
            "`--port` must be a number from 0 to 65535, not `65536`"
        );
        assert_eq!(
            error("--port -1"),
            "`--port` must be a number from 0 to 65535, not `-1`"
        );
        assert_eq!(error("--port 1 --port 2"), "`--port` given twice");
        assert_eq!(
            error("--token-file a --token-file b"),
            "`--token-file` given twice"
        );
    }

    #[test]
    fn unknown_arguments_are_usage_errors() {
        assert_eq!(error("--bogus"), "unknown option `--bogus`");
        assert_eq!(error("serve"), "unexpected argument `serve`");
    }

    /// ARCH plans `--collection`; until the engine persists anything it is refused by
    /// name rather than reported as unknown.
    #[test]
    fn collection_is_not_supported_yet() {
        assert!(
            error("--collection scenes.json").starts_with("`--collection` is not supported yet")
        );
    }
}
