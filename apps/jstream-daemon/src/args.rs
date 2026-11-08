//! The command line: a small hand-rolled parser, like `jstream-cli`'s
//! (apps/jstream-daemon/README.md#design says why there is no parser crate).
//!
//! The parser is pure: the `JSTREAM_TOKEN_FILE` fallback is read by `main`, not here.

use jstream_protocol::OutputId;
use std::net::IpAddr;

/// The network listener's default port (ADR-0104 §1): fixed, so a paired device finds
/// the host again, and next to the desktop app's `--mcp` port (4455).
pub const LAN_PORT: u16 = 4456;

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
  --collection <path>  load the collection from this JSON file and save every
                       change to it; created, with only four sample labels,
                       if it doesn't exist
                       (default: none, the collection starts empty every run)
  --output <id>        add a stopped placeholder output (repeatable)
  --plugin-dir <dir>   where installed WASM plugins and their grants are saved
                       (plugins.json) and loaded from at startup (default:
                       `plugins` in the platform data directory); created if
                       missing
  --effects-dir <dir>  the folder of WGSL effect packs to load, one pack folder
                       each, and reload when they change (default: `effects` in
                       the platform data directory)
  --lan                also listen for devices on the network (ADR-0104): a
                       second listener, TLS only, with a self-signed
                       certificate made on first use and kept beside the token
                       file (<token-file>.tls). It admits paired devices only,
                       never the control token. Its addresses and the
                       certificate's fingerprint are printed at startup: a
                       device checks the fingerprint once, then pairs with a
                       code from `jstream-cli pair`. It listens on every
                       interface, so also on public Wi-Fi or a VPN this
                       machine joins; --lan-bind picks one
  --lan-port <port>    the network listener's port (default: 4456)
  --lan-bind <addr>    listen on this one address instead of every interface
  --lan-renew          with --lan: make a new key and certificate before
                       serving, for when the machine's names or addresses
                       changed. Every paired device has to check the new
                       fingerprint, and a native client that pinned the old
                       one has to pair again
  --jtunnel-accept <name@address:port>
                       receive from this JTunnel sender (ADR-0123): a DAW
                       plugin's tunnel name and the LAN address and port it
                       sends from (repeatable). Off without it: no socket is
                       open. A `network_audio` source naming the sender then
                       records its audio
  --ui-dir <dir>       serve the web UI's built files from this directory at
                       http://127.0.0.1:<port>/ (the browser asks for the token
                       as a password)
  --log-dir <dir>      where log files, crash reports and the audit log
                       (audit.jsonl) go (default: `logs` in the platform data
                       directory); created if missing
  --log-level <spec>   the log filter, as `target=level` directives and an
                       optional default level (default: RUST_LOG, else info)
  --youtube-client <path>
                       Windows, for developers: a Google OAuth client to use
                       for YouTube sign-in instead of JStream's built-in one,
                       the JSON Google Cloud gives for a Desktop app client
                       (default: JSTREAM_YOUTUBE_CLIENT_ID and
                       JSTREAM_YOUTUBE_CLIENT_SECRET, else the built-in client)
  --register-vcam      Windows, optional: register the virtual camera for this
                       machine, then exit. Copies the DLL beside the daemon to
                       an administrators-only folder under Program Files and
                       registers that copy; run it again after an update to
                       refresh the copy. Asks for administrator rights once;
                       takes no other options
  --unregister-vcam    Windows: remove the virtual camera's registration and its
                       copy, then exit. Asks for administrator rights once;
                       takes no other options
  -V, --version        print the daemon and protocol versions
  -h, --help           print this help

It prints `mcp endpoint: http://127.0.0.1:<port>/mcp` (MCP over streamable HTTP,
with the token as `Authorization: Bearer`), `ui: http://127.0.0.1:<port>/` with
--ui-dir, then `listening on
ws://127.0.0.1:<port>/api/v1/ws` once it serves, and runs until Ctrl-C (or SIGTERM).
Clients: jstream-cli --url <that URL> --token-file <path>.

Exit codes: 0 stopped by a signal, 1 failed to start or serve, 2 usage,
3 Media Foundation or its AAC encoder is missing (Windows N editions).
";

/// What the daemon serves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--token-file`. Without it `main` reads `JSTREAM_TOKEN_FILE`.
    pub token_file: Option<String>,
    /// `--port`: `0` lets the OS pick a free one.
    pub port: u16,
    /// `--collection`: the collection file. Without it the collection is not saved.
    pub collection: Option<String>,
    /// `--output`: placeholder outputs, in the order given.
    pub outputs: Vec<OutputId>,
    /// `--log-dir`. Without it `main` uses `logs` in the platform data directory.
    pub log_dir: Option<String>,
    /// `--log-level`, unparsed: `main` parses it, and reads `RUST_LOG` without it.
    pub log_level: Option<String>,
    /// `--youtube-client`: a developer's OAuth client file, overriding the built-in
    /// client. Without it `main` reads `JSTREAM_YOUTUBE_CLIENT_ID` and
    /// `JSTREAM_YOUTUBE_CLIENT_SECRET`.
    pub youtube_client: Option<String>,
    /// `--ui-dir`: the web UI's built files. Without it no UI is served.
    pub ui_dir: Option<String>,
    /// `--plugin-dir`: where installed WASM plugins are saved. Without it `main` uses
    /// `plugins` in the platform data directory; left out entirely, plugins aren't saved.
    pub plugin_dir: Option<String>,
    /// `--effects-dir`: the folder of WGSL effect packs. Without it `main` uses
    /// `effects` in the platform data directory; left out entirely, no packs load.
    pub effects_dir: Option<String>,
    /// `--lan` and its options: the network listener, off without it.
    pub lan: Option<Lan>,
    /// `--jtunnel-accept`: the JTunnel senders to receive from, in the order given.
    /// None turns the receiver off.
    pub jtunnel: Vec<jstream_core::AcceptedSender>,
}

/// The network listener's options (`--lan`, ADR-0104).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lan {
    /// `--lan-port`, [`LAN_PORT`] by default.
    pub port: u16,
    /// `--lan-bind`: one address to listen on; every interface without it.
    pub bind: Option<IpAddr>,
    /// `--lan-renew`: make a new key and certificate first.
    pub renew: bool,
}

/// A parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    /// Print the help text.
    Help,
    /// Print the versions.
    Version,
    /// Register the virtual camera, then exit.
    RegisterVcam,
    /// Unregister the virtual camera and delete its copy, then exit.
    UnregisterVcam,
    /// Run the engine and serve it. Boxed: the options dwarf the other variants.
    Serve(Box<Options>),
}

/// Parses the arguments after the program name. The error is a usage message.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut options = Options::default();
    let mut port = None;
    let mut lan = false;
    let mut lan_port = None;
    let mut lan_bind = None;
    let mut lan_renew = false;
    let mut register_vcam = false;
    let mut unregister_vcam = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().ok_or_else(|| format!("`{arg}` needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            "--register-vcam" => register_vcam = true,
            "--unregister-vcam" => unregister_vcam = true,
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
            "--lan" => lan = true,
            "--lan-renew" => lan_renew = true,
            "--lan-port" => {
                let text = value()?;
                let parsed = text
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| {
                        format!("`--lan-port` must be a number from 1 to 65535, not `{text}`")
                    })?;
                if lan_port.replace(parsed).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--lan-bind" => {
                let text = value()?;
                let parsed = text.parse::<IpAddr>().map_err(|_| {
                    format!(
                        "`--lan-bind` must be an IP address, such as 192.168.1.20, not `{text}`"
                    )
                })?;
                if parsed.is_loopback() {
                    return Err(
                        "`--lan-bind` can't be a loopback address: the host's own listener is there"
                            .to_owned(),
                    );
                }
                if lan_bind.replace(parsed).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--output" => options.outputs.push(OutputId::new(value()?.as_str())),
            "--jtunnel-accept" => {
                let text = value()?;
                let sender = text
                    .parse::<jstream_core::AcceptedSender>()
                    .map_err(|err| format!("`--jtunnel-accept`: {err}"))?;
                if options.jtunnel.iter().any(|s| s.name() == sender.name()) {
                    return Err(format!(
                        "`--jtunnel-accept` names the sender `{}` twice",
                        sender.name()
                    ));
                }
                options.jtunnel.push(sender);
            }
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
            "--youtube-client" => {
                let path = value()?;
                if options.youtube_client.replace(path.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--collection" => {
                let path = value()?;
                if options.collection.replace(path.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--ui-dir" => {
                let dir = value()?;
                if options.ui_dir.replace(dir.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--plugin-dir" => {
                let dir = value()?;
                if options.plugin_dir.replace(dir.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            "--effects-dir" => {
                let dir = value()?;
                if options.effects_dir.replace(dir.clone()).is_some() {
                    return Err(format!("`{arg}` given twice"));
                }
            }
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    if register_vcam {
        if args.len() > 1 {
            return Err("`--register-vcam` takes no other options".to_owned());
        }
        return Ok(Invocation::RegisterVcam);
    }
    if unregister_vcam {
        if args.len() > 1 {
            return Err("`--unregister-vcam` takes no other options".to_owned());
        }
        return Ok(Invocation::UnregisterVcam);
    }
    options.port = port.unwrap_or(0);
    if lan {
        options.lan = Some(Lan {
            port: lan_port.unwrap_or(LAN_PORT),
            bind: lan_bind,
            renew: lan_renew,
        });
    } else if lan_port.is_some() || lan_bind.is_some() || lan_renew {
        return Err("`--lan-port`, `--lan-bind` and `--lan-renew` need `--lan`".to_owned());
    }
    Ok(Invocation::Serve(Box::new(options)))
}

#[cfg(test)]
mod tests {
    use super::{Invocation, LAN_PORT, Lan, Options, parse};
    use jstream_protocol::OutputId;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn serve(line: &str) -> Options {
        match parse(&args(line)) {
            Ok(Invocation::Serve(options)) => *options,
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

    /// ADR-0123 §1: JTunnel is off unless a sender is accepted, each a LAN address.
    #[test]
    fn jtunnel_is_off_until_a_sender_is_accepted() {
        assert!(serve("").jtunnel.is_empty());
        let options =
            serve("--jtunnel-accept Bitwig@192.168.1.5:9000 --jtunnel-accept Live@10.0.0.2:9000");
        let names: Vec<&str> = options.jtunnel.iter().map(|s| s.name()).collect();
        assert_eq!(names, ["Bitwig", "Live"]);
        assert!(error("--jtunnel-accept Bitwig@8.8.8.8:9000").contains("not a LAN address"));
        assert!(error("--jtunnel-accept Bitwig").contains("name@address:port"));
        assert!(
            error("--jtunnel-accept A@10.0.0.1:1 --jtunnel-accept A@10.0.0.2:1").contains("twice")
        );
    }

    /// ADR-0104 §1: network access is off unless `--lan` turns it on, on a fixed port.
    #[test]
    fn the_network_listener_is_opt_in() {
        assert_eq!(serve("").lan, None);
        assert_eq!(
            serve("--lan").lan,
            Some(Lan {
                port: LAN_PORT,
                bind: None,
                renew: false
            })
        );
        assert_eq!(
            serve("--lan-port 5000 --lan --lan-bind 192.168.1.20 --lan-renew").lan,
            Some(Lan {
                port: 5000,
                bind: Some([192, 168, 1, 20].into()),
                renew: true
            })
        );
        assert_eq!(
            error("--lan-port 5000"),
            "`--lan-port`, `--lan-bind` and `--lan-renew` need `--lan`"
        );
        assert_eq!(
            error("--lan --lan-port 0"),
            "`--lan-port` must be a number from 1 to 65535, not `0`"
        );
        assert!(error("--lan --lan-bind studio").contains("must be an IP address"));
        assert!(error("--lan --lan-bind 127.0.0.1").contains("loopback"));
        assert_eq!(
            error("--lan --lan-port 1 --lan-port 2"),
            "`--lan-port` given twice"
        );
    }

    #[test]
    fn options_are_read_in_any_order() {
        let options = serve(
            "--output out_1 --log-level debug --port 4455 --token-file /tmp/t \
             --collection /tmp/c.json --log-dir /tmp/logs --output out_2 --ui-dir /tmp/dist \
             --youtube-client /tmp/yt.json --plugin-dir /tmp/plugins \
             --effects-dir /tmp/effects",
        );
        assert_eq!(
            options,
            Options {
                token_file: Some("/tmp/t".to_owned()),
                port: 4455,
                collection: Some("/tmp/c.json".to_owned()),
                outputs: vec![OutputId::new("out_1"), OutputId::new("out_2")],
                log_dir: Some("/tmp/logs".to_owned()),
                log_level: Some("debug".to_owned()),
                youtube_client: Some("/tmp/yt.json".to_owned()),
                ui_dir: Some("/tmp/dist".to_owned()),
                plugin_dir: Some("/tmp/plugins".to_owned()),
                effects_dir: Some("/tmp/effects".to_owned()),
                lan: None,
                jtunnel: Vec::new(),
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
        assert_eq!(error("--collection"), "`--collection` needs a value");
        assert_eq!(error("--ui-dir"), "`--ui-dir` needs a value");
        assert_eq!(error("--ui-dir a --ui-dir b"), "`--ui-dir` given twice");
        assert_eq!(
            error("--plugin-dir a --plugin-dir b"),
            "`--plugin-dir` given twice"
        );
        assert_eq!(
            error("--youtube-client"),
            "`--youtube-client` needs a value"
        );
        assert_eq!(
            error("--youtube-client a --youtube-client b"),
            "`--youtube-client` given twice"
        );
        assert_eq!(
            error("--collection a.json --collection b.json"),
            "`--collection` given twice"
        );
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

    #[test]
    fn register_vcam_stands_alone() {
        assert_eq!(
            parse(&args("--register-vcam")),
            Ok(Invocation::RegisterVcam)
        );
        assert_eq!(
            error("--register-vcam --port 1"),
            "`--register-vcam` takes no other options"
        );
        assert_eq!(
            error("--register-vcam --register-vcam"),
            "`--register-vcam` takes no other options"
        );
        assert_eq!(parse(&args("--register-vcam --help")), Ok(Invocation::Help));
    }

    #[test]
    fn unregister_vcam_stands_alone() {
        assert_eq!(
            parse(&args("--unregister-vcam")),
            Ok(Invocation::UnregisterVcam)
        );
        assert_eq!(
            error("--unregister-vcam --port 1"),
            "`--unregister-vcam` takes no other options"
        );
        assert_eq!(
            error("--register-vcam --unregister-vcam"),
            "`--register-vcam` takes no other options"
        );
        assert_eq!(
            parse(&args("--unregister-vcam --help")),
            Ok(Invocation::Help)
        );
    }
}
