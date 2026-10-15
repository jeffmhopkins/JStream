//! The command line: a small hand-rolled parser (apps/jstream-cli/README.md#design says
//! why there is no parser crate).
//!
//! Shape: `jstream-cli [global options] <command> [command options] [arguments]`. Global
//! options come before the command. Edits are named by their protocol command names
//! (`scene.create`), so the CLI maps one to one onto the protocol (R-ARCH-001).

use crate::exit::CliError;
use jstream_control::protocol::{
    Action, Batch, Crop, Edit, Finite32, FitMode, IdempotencyKey, ItemGeometry, ItemId,
    ItemTransform, OutputId, OutputKind, Query, Rev, RtmpTarget, SceneId, SceneRole, Size,
    SourceId, SourceKind, TransitionPresetId, Vec2,
};

/// The help text, printed by `help`, `--help` and `-h`.
pub const USAGE: &str = "\
jstream-cli: drive a JStream engine from the command line

Usage: jstream-cli [global options] <command> [arguments]

Global options (before the command):
  --json               machine output: one JSON value per line on stdout,
                       errors as JSON on stderr
  --url <url>          the engine's WebSocket API, `ws://<host>:<port>` (the path
                       defaults to /api/v1/ws); plain ws:// only, no TLS yet
  --token-file <path>  the file holding the control token (default: the
                       JSTREAM_TOKEN_FILE environment variable)
  --mock               use an in-memory mock engine instead; it starts empty at
                       rev 0 for each run. Needs a build with the `mock` cargo
                       feature
  --mock-output <id>   with --mock: add a stopped output (repeatable)
  -h, --help           print this help

Commands:
  version                          print the CLI and protocol versions (no connection)
  hello                            handshake with the engine and print the session
  state get                        print the whole collection state (query state.get)
  scene list                       list the scenes (query scene.list)
  scene switch <scene-id> [--preset <preset-id>]
                                   put a scene on program, with its transition
                                   (action scene.switch)
  studio get                       print the scenes on program and preview (query
                                   studio.get)
  studio set-preview <scene-id>    put a scene on preview (action studio.set_preview)
  source list                      list the sources (query source.list)
  item list <scene-id>             list a scene's items, bottom first (query item.list)
  output list                      list the configured outputs (query output.list)
  output status <output-id>        report an output's state and whether its stream
                                   key is set (query output.status)
  video snapshot                   the program mix as a PNG (query video.snapshot)
  output start <output-id>         start an output (action output.start)
  output stop <output-id>          stop an output (action output.stop)
  output set-key <output-id> [--key-file <path>]
                                   store the output's stream key (action
                                   output.set_key), read from the file or else from
                                   stdin; never from the command line
  output clear-key <output-id>     remove the output's stream key (action
                                   output.clear_key)
  edit [--expected-rev <rev>] [--idempotency-key <key>] <edit>...
                                   apply the edits as one atomic batch
  events [--count <n>]             print events as they arrive; stop after <n>
  mcp                              serve MCP on stdin/stdout (one JSON-RPC message
                                   per line) over this connection, until stdin ends
  help                             print this help

Edits (each is its protocol command name followed by its arguments):
  scene.create <scene-id> <name>
  scene.rename <scene-id> <name>
  scene.delete <scene-id>
  scene.reorder <scene-id> <index>
  scene.set_role <scene-id> scene|group
  source.create <source-id> <name> builtin:<kind> | scene:<scene-id>
  source.rename <source-id> <name>
  source.duplicate <source-id> <new-source-id> <name>
  source.delete <source-id>
  item.add <scene-id> <item-id> <source-id>
  item.remove <item-id>
  item.reorder <item-id> <index>
  item.move <item-id> <scene-id> <index>
  item.set_visible <item-id> true|false
  item.set_source <item-id> <source-id>
  item.set_crop <item-id> <left> <top> <right> <bottom>
  item.set_transform <item-id> <x> <y> <scale-x> <scale-y> <rotation-deg>
  item.set_geometry <item-id> <center-x> <center-y> <width> <height> <rotation-deg>
                    stretch|fit|fill
  canvas.set_size <width> <height>
  output.create <output-id> <rtmp-url>
  output.set_target <output-id> <rtmp-url>
  output.delete <output-id>
  An <rtmp-url> is the ingest server, rtmp://host[:port]/app or rtmps://...,
  without the stream key: set that with `output set-key`.

Exit codes: 0 ok, 1 output error, 2 usage, 3 disconnected, 10-20 engine errors
(see the README for the full table).
";

/// Where the CLI gets its `Control` session from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    /// The engine over its WebSocket API.
    Remote {
        /// `--url`: the engine's WebSocket URL. Required to connect.
        url: Option<String>,
        /// `--token-file`: the file holding the control token. Without it the connector
        /// reads `JSTREAM_TOKEN_FILE`.
        token_file: Option<String>,
    },
    /// A fresh in-memory `MockEngine` with these outputs, all stopped.
    Mock {
        /// Outputs to create before connecting.
        outputs: Vec<OutputId>,
    },
}

/// A parsed command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    /// `--json`: machine output.
    pub json: bool,
    /// Which engine to talk to.
    pub connection: Connection,
    /// What to do.
    pub command: Command,
}

/// One CLI command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Print the help text.
    Help,
    /// Print the CLI and protocol versions without connecting.
    Version,
    /// Handshake and print the welcome.
    Hello,
    /// Run a query.
    Query(Query),
    /// Run an action.
    Action(Action),
    /// Run `output.set_key`, with the key read when the command runs: from `key_file`,
    /// or else from the input (stdin). The key is never an argument, so it stays out of
    /// shell history and the process list.
    SetKey {
        /// The output whose key to set.
        output_id: OutputId,
        /// `--key-file`: the file holding the key.
        key_file: Option<String>,
    },
    /// Apply a batch of edits.
    Edit(Batch),
    /// Print events, stopping after `count` if given.
    Events {
        /// How many events to print before exiting.
        count: Option<u64>,
    },
    /// Serve MCP on stdin and stdout over the session, until stdin ends.
    Mcp,
}

fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

/// Whether the global options ask for `--json`, read the way [`parse`] reads them (the
/// values after `--mock-output`, `--url` and `--token-file` are skipped; reading stops at
/// the command, at `--help` and
/// at an unknown option). Lets a usage error be reported as JSON when parsing fails.
pub fn json_requested(args: &[String]) -> bool {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => return true,
            "--mock" => {}
            "--mock-output" | "--url" | "--token-file" => {
                rest.next();
            }
            _ => return false,
        }
    }
    false
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Invocation, CliError> {
    let mut json = false;
    let mut mock = false;
    let mut outputs = Vec::new();
    let mut url = None;
    let mut token_file = None;
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        if !first.starts_with('-') {
            break;
        }
        rest = tail;
        match first.as_str() {
            "--json" => json = true,
            "--mock" => mock = true,
            "--mock-output" => {
                let (id, tail) = rest
                    .split_first()
                    .ok_or_else(|| usage("`--mock-output` needs an output ID"))?;
                outputs.push(OutputId::new(id.as_str()));
                rest = tail;
            }
            "--url" | "--token-file" => {
                let (value, tail) = rest
                    .split_first()
                    .ok_or_else(|| usage(format!("`{first}` needs a value")))?;
                let slot = if first == "--url" {
                    &mut url
                } else {
                    &mut token_file
                };
                if slot.replace(value.clone()).is_some() {
                    return Err(usage(format!("`{first}` given twice")));
                }
                rest = tail;
            }
            "-h" | "--help" => {
                return Ok(Invocation {
                    json,
                    connection: Connection::Remote {
                        url: None,
                        token_file: None,
                    },
                    command: Command::Help,
                });
            }
            other => return Err(usage(format!("unknown option `{other}`"))),
        }
    }
    if !mock && !outputs.is_empty() {
        return Err(usage("`--mock-output` needs `--mock`"));
    }
    if mock && (url.is_some() || token_file.is_some()) {
        return Err(usage(
            "`--mock` can't be combined with `--url` or `--token-file`",
        ));
    }
    let connection = if mock {
        Connection::Mock { outputs }
    } else {
        Connection::Remote { url, token_file }
    };
    let command = parse_command(rest)?;
    Ok(Invocation {
        json,
        connection,
        command,
    })
}

fn parse_command(args: &[String]) -> Result<Command, CliError> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let command = match words.as_slice() {
        [] => return Err(usage("missing command; try `jstream-cli help`")),
        ["help"] => Command::Help,
        ["version"] => Command::Version,
        ["hello"] => Command::Hello,
        ["state", "get"] => Command::Query(Query::StateGet),
        ["scene", "list"] => Command::Query(Query::SceneList),
        ["scene", "switch", id] => Command::Action(Action::SceneSwitch {
            scene_id: SceneId::new(*id),
            preset_id: None,
        }),
        ["scene", "switch", id, "--preset", preset] => Command::Action(Action::SceneSwitch {
            scene_id: SceneId::new(*id),
            preset_id: Some(TransitionPresetId::new(*preset)),
        }),
        ["studio", "get"] => Command::Query(Query::StudioGet),
        ["studio", "set-preview", id] => Command::Action(Action::StudioSetPreview {
            scene_id: SceneId::new(*id),
        }),
        ["source", "list"] => Command::Query(Query::SourceList),
        ["item", "list", id] => Command::Query(Query::ItemList {
            scene_id: SceneId::new(*id),
        }),
        ["output", "status", id] => Command::Query(Query::OutputStatus {
            output_id: OutputId::new(*id),
        }),
        ["video", "snapshot"] => Command::Query(Query::VideoSnapshot),
        ["output", "start", id] => Command::Action(Action::OutputStart {
            output_id: OutputId::new(*id),
        }),
        ["output", "stop", id] => Command::Action(Action::OutputStop {
            output_id: OutputId::new(*id),
        }),
        ["output", "list"] => Command::Query(Query::OutputList),
        ["output", "set-key", id, rest @ ..] => parse_set_key(id, rest)?,
        ["output", "clear-key", id] => Command::Action(Action::OutputClearKey {
            output_id: OutputId::new(*id),
        }),
        ["edit", rest @ ..] => Command::Edit(parse_batch(rest)?),
        ["events", rest @ ..] => parse_events(rest)?,
        ["mcp"] => Command::Mcp,
        [word, ..] => {
            let known = [
                "help", "version", "hello", "state", "scene", "source", "item", "output", "video",
                "studio", "edit", "events", "mcp",
            ];
            return Err(if known.contains(word) {
                usage(format!(
                    "wrong arguments for `{word}`; try `jstream-cli help`"
                ))
            } else {
                usage(format!("unknown command `{word}`; try `jstream-cli help`"))
            });
        }
    };
    Ok(command)
}

/// `output set-key <id> [--key-file <path>]`. Anything else after the ID is refused
/// without being repeated, since it may be the key itself.
fn parse_set_key(id: &str, args: &[&str]) -> Result<Command, CliError> {
    let key_file = match args {
        [] => None,
        ["--key-file", path] => Some((*path).to_owned()),
        ["--key-file"] => return Err(usage("`--key-file` needs a path")),
        _ => {
            return Err(usage(
                "`output set-key` reads the key from stdin or `--key-file <path>`, \
                 never from the command line",
            ));
        }
    };
    Ok(Command::SetKey {
        output_id: OutputId::new(id),
        key_file,
    })
}

fn parse_events(mut args: &[&str]) -> Result<Command, CliError> {
    let mut count = None;
    while let Some((&option, rest)) = args.split_first() {
        match (option, rest) {
            ("--count", [n, rest @ ..]) => {
                count = Some(number::<u64>(n, "--count")?);
                args = rest;
            }
            _ => return Err(usage(format!("unexpected `{option}` after `events`"))),
        }
    }
    Ok(Command::Events { count })
}

fn parse_batch(mut args: &[&str]) -> Result<Batch, CliError> {
    let mut expected_rev = None;
    let mut idempotency_key = None;
    loop {
        match args {
            ["--expected-rev", rev, rest @ ..] => {
                expected_rev = Some(Rev(number(rev, "--expected-rev")?));
                args = rest;
            }
            ["--idempotency-key", key, rest @ ..] => {
                idempotency_key = Some(IdempotencyKey::new(*key));
                args = rest;
            }
            [option @ ("--expected-rev" | "--idempotency-key")] => {
                return Err(usage(format!("`{option}` needs a value")));
            }
            _ => break,
        }
    }
    let mut edits = Vec::new();
    while !args.is_empty() {
        let (edit, rest) = parse_edit(args)?;
        edits.push(edit);
        args = rest;
    }
    if edits.is_empty() {
        return Err(usage(
            "`edit` needs at least one edit; try `jstream-cli help`",
        ));
    }
    Ok(Batch {
        edits,
        expected_rev,
        idempotency_key,
    })
}

/// How many arguments each edit takes, after its name.
fn arity(name: &str) -> Option<usize> {
    Some(match name {
        "scene.delete" | "source.delete" | "item.remove" | "output.delete" => 1,
        "scene.create" | "scene.rename" | "scene.reorder" | "scene.set_role" | "source.rename"
        | "item.reorder" | "item.set_visible" | "item.set_source" | "output.create"
        | "output.set_target" | "canvas.set_size" => 2,
        "source.create" | "source.duplicate" | "item.add" | "item.move" => 3,
        "item.set_crop" => 5,
        "item.set_transform" => 6,
        "item.set_geometry" => 7,
        _ => return None,
    })
}

/// Parses one edit off the front of `args`, returning it and the arguments after it.
fn parse_edit<'a, 'b>(args: &'a [&'b str]) -> Result<(Edit, &'a [&'b str]), CliError> {
    let Some((&name, rest)) = args.split_first() else {
        return Err(usage("missing edit"));
    };
    let Some(n) = arity(name) else {
        return Err(usage(format!(
            "unknown edit `{name}`; edits are protocol command names such as `scene.create`"
        )));
    };
    if rest.len() < n {
        return Err(usage(format!(
            "`{name}` takes {n} argument{}; try `jstream-cli help`",
            if n == 1 { "" } else { "s" }
        )));
    }
    let (a, rest) = rest.split_at(n);
    let edit = match (name, a) {
        ("scene.create", [id, name]) => Edit::SceneCreate {
            scene_id: SceneId::new(*id),
            name: (*name).to_owned(),
        },
        ("scene.rename", [id, name]) => Edit::SceneRename {
            scene_id: SceneId::new(*id),
            name: (*name).to_owned(),
        },
        ("scene.delete", [id]) => Edit::SceneDelete {
            scene_id: SceneId::new(*id),
        },
        ("scene.reorder", [id, index]) => Edit::SceneReorder {
            scene_id: SceneId::new(*id),
            index: number(index, "index")?,
        },
        ("scene.set_role", [id, role]) => Edit::SceneSetRole {
            scene_id: SceneId::new(*id),
            role: match *role {
                "scene" => SceneRole::Scene,
                "group" => SceneRole::Group,
                other => {
                    return Err(usage(format!(
                        "`scene.set_role` takes `scene` or `group`, not `{other}`"
                    )));
                }
            },
        },
        ("source.create", [id, name, kind]) => Edit::SourceCreate {
            source_id: SourceId::new(*id),
            name: (*name).to_owned(),
            kind: source_kind(kind)?,
            settings: Default::default(),
            duplicated_from: None,
        },
        ("source.rename", [id, name]) => Edit::SourceRename {
            source_id: SourceId::new(*id),
            name: (*name).to_owned(),
        },
        ("source.duplicate", [id, new_id, name]) => Edit::SourceDuplicate {
            source_id: SourceId::new(*id),
            new_source_id: SourceId::new(*new_id),
            name: (*name).to_owned(),
        },
        ("source.delete", [id]) => Edit::SourceDelete {
            source_id: SourceId::new(*id),
        },
        ("item.add", [scene, item, source]) => Edit::ItemAdd {
            scene_id: SceneId::new(*scene),
            item_id: ItemId::new(*item),
            source_id: SourceId::new(*source),
            transform: None,
            geometry: None,
        },
        ("item.remove", [id]) => Edit::ItemRemove {
            item_id: ItemId::new(*id),
        },
        ("item.set_source", [id, source]) => Edit::ItemSetSource {
            item_id: ItemId::new(*id),
            source_id: SourceId::new(*source),
        },
        ("item.reorder", [id, index]) => Edit::ItemReorder {
            item_id: ItemId::new(*id),
            index: number(index, "index")?,
        },
        ("item.move", [id, scene, index]) => Edit::ItemMove {
            item_id: ItemId::new(*id),
            scene_id: SceneId::new(*scene),
            index: number(index, "index")?,
        },
        ("item.set_visible", [id, visible]) => Edit::ItemSetVisible {
            item_id: ItemId::new(*id),
            scene_id: None,
            visible: match *visible {
                "true" => true,
                "false" => false,
                other => {
                    return Err(usage(format!(
                        "`item.set_visible` takes `true` or `false`, not `{other}`"
                    )));
                }
            },
        },
        ("item.set_crop", [id, left, top, right, bottom]) => Edit::ItemSetCrop {
            item_id: ItemId::new(*id),
            scene_id: None,
            crop: Crop {
                left: number(left, "left")?,
                top: number(top, "top")?,
                right: number(right, "right")?,
                bottom: number(bottom, "bottom")?,
            },
        },
        ("item.set_transform", [id, x, y, sx, sy, rot]) => Edit::ItemSetTransform {
            item_id: ItemId::new(*id),
            scene_id: None,
            transform: ItemTransform {
                position: vec2(x, y)?,
                scale: vec2(sx, sy)?,
                rotation_deg: finite(rot, "rotation-deg")?,
            },
        },
        ("item.set_geometry", [id, x, y, width, height, rot, fit]) => Edit::ItemSetGeometry {
            item_id: ItemId::new(*id),
            scene_id: None,
            // The 3D fields stay neutral: the engine rejects anything else until the
            // compositor draws 3D (ADR-0022).
            geometry: ItemGeometry {
                center: Vec2 {
                    x: finite(x, "center-x")?,
                    y: finite(y, "center-y")?,
                },
                size: Size {
                    width: finite(width, "width")?,
                    height: finite(height, "height")?,
                },
                fit_mode: match *fit {
                    "stretch" => FitMode::Stretch,
                    "fit" => FitMode::Fit,
                    "fill" => FitMode::Fill,
                    other => {
                        return Err(usage(format!(
                            "`item.set_geometry` takes `stretch`, `fit` or `fill`, not `{other}`"
                        )));
                    }
                },
                rotation_deg: finite(rot, "rotation-deg")?,
                tilt_deg: Finite32::ZERO,
                turn_deg: Finite32::ZERO,
                depth_px: Finite32::ZERO,
                perspective_px: None,
            },
        },
        ("canvas.set_size", [width, height]) => Edit::CanvasSetSize {
            width: number(width, "width")?,
            height: number(height, "height")?,
        },
        ("output.create", [id, url]) => Edit::OutputCreate {
            output_id: OutputId::new(*id),
            kind: OutputKind::Rtmp {
                target: rtmp_target(url)?,
            },
        },
        ("output.set_target", [id, url]) => Edit::OutputSetTarget {
            output_id: OutputId::new(*id),
            target: rtmp_target(url)?,
        },
        ("output.delete", [id]) => Edit::OutputDelete {
            output_id: OutputId::new(*id),
        },
        _ => return Err(usage(format!("cannot parse `{name}`"))),
    };
    Ok((edit, rest))
}

fn source_kind(text: &str) -> Result<SourceKind, CliError> {
    if let Some(kind) = text.strip_prefix("builtin:") {
        Ok(SourceKind::Builtin {
            kind: kind.to_owned(),
        })
    } else if let Some(scene) = text.strip_prefix("scene:") {
        Ok(SourceKind::Scene {
            scene_id: SceneId::new(scene),
        })
    } else {
        Err(usage(format!(
            "source kind `{text}` must be `builtin:<kind>` or `scene:<scene-id>`"
        )))
    }
}

/// An RTMP server URL as an output target. Only generic RTMP(S) targets exist, so the
/// URL is the whole target; the engine validates it. A URL without an `rtmp` or `rtmps`
/// scheme is refused here, which leaves other spellings free for later output kinds.
/// The message doesn't repeat the argument, which may hold a key.
fn rtmp_target(url: &str) -> Result<RtmpTarget, CliError> {
    let scheme = url.split_once("://").map(|(scheme, _)| scheme);
    if !scheme.is_some_and(|s| s.eq_ignore_ascii_case("rtmp") || s.eq_ignore_ascii_case("rtmps")) {
        return Err(usage(
            "an output target must be an `rtmp://` or `rtmps://` server URL",
        ));
    }
    Ok(RtmpTarget::Generic {
        url: url.to_owned(),
    })
}

fn number<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, CliError> {
    text.parse().map_err(|_| {
        usage(format!(
            "`{what}` must be a non-negative integer, not `{text}`"
        ))
    })
}

fn finite(text: &str, what: &str) -> Result<Finite32, CliError> {
    text.parse::<f32>()
        .ok()
        .and_then(Finite32::new)
        .ok_or_else(|| usage(format!("`{what}` must be a finite number, not `{text}`")))
}

fn vec2(x: &str, y: &str) -> Result<Vec2, CliError> {
    Ok(Vec2 {
        x: finite(x, "x")?,
        y: finite(y, "y")?,
    })
}

#[cfg(test)]
mod tests {
    use super::{Command, Connection, Invocation, parse};
    use crate::exit::CliError;
    use jstream_control::protocol::{
        Action, Batch, Crop, Edit, FitMode, IdempotencyKey, ItemId, OutputId, OutputKind, Query,
        Rev, RtmpTarget, SceneId, SceneRole, SourceId, SourceKind, TransitionPresetId,
    };

    fn args(line: &[&str]) -> Vec<String> {
        line.iter().map(|s| (*s).to_owned()).collect()
    }

    fn ok(line: &[&str]) -> Invocation {
        parse(&args(line)).expect("parses")
    }

    fn err(line: &[&str]) -> String {
        match parse(&args(line)) {
            Err(CliError::Usage(message)) => message,
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn global_options_and_simple_commands() {
        assert_eq!(
            ok(&["--json", "--mock", "--mock-output", "out_1", "hello"]),
            Invocation {
                json: true,
                connection: Connection::Mock {
                    outputs: vec![OutputId::new("out_1")]
                },
                command: Command::Hello,
            }
        );
        let plain = ok(&["scene", "list"]);
        assert!(!plain.json);
        assert_eq!(
            plain.connection,
            Connection::Remote {
                url: None,
                token_file: None
            }
        );
        assert_eq!(plain.command, Command::Query(Query::SceneList));
        assert_eq!(ok(&["version"]).command, Command::Version);
        assert_eq!(ok(&["help"]).command, Command::Help);
        assert_eq!(ok(&["--help"]).command, Command::Help);
        assert_eq!(ok(&["--mock", "-h", "junk"]).command, Command::Help);
    }

    #[test]
    fn read_commands_map_to_queries() {
        assert_eq!(
            ok(&["state", "get"]).command,
            Command::Query(Query::StateGet)
        );
        assert_eq!(
            ok(&["source", "list"]).command,
            Command::Query(Query::SourceList)
        );
        assert_eq!(
            ok(&["item", "list", "main"]).command,
            Command::Query(Query::ItemList {
                scene_id: SceneId::new("main")
            })
        );
        for wrong in [
            &["item", "list"][..],
            &["item", "list", "a", "b"],
            &["source"],
            &["state"],
        ] {
            assert!(err(wrong).starts_with("wrong arguments for `"), "{wrong:?}");
        }
    }

    #[test]
    fn output_commands_map_to_queries_and_actions() {
        let id = || OutputId::new("o");
        assert_eq!(
            ok(&["output", "status", "o"]).command,
            Command::Query(Query::OutputStatus { output_id: id() })
        );
        assert_eq!(
            ok(&["output", "start", "o"]).command,
            Command::Action(Action::OutputStart { output_id: id() })
        );
        assert_eq!(
            ok(&["output", "stop", "o"]).command,
            Command::Action(Action::OutputStop { output_id: id() })
        );
        assert_eq!(
            ok(&["video", "snapshot"]).command,
            Command::Query(Query::VideoSnapshot)
        );
        assert!(err(&["video"]).starts_with("wrong arguments for `"));
        assert!(err(&["video", "snapshot", "extra"]).starts_with("wrong arguments for `"));
        assert_eq!(
            ok(&["output", "list"]).command,
            Command::Query(Query::OutputList)
        );
        assert_eq!(
            ok(&["output", "clear-key", "o"]).command,
            Command::Action(Action::OutputClearKey { output_id: id() })
        );
        assert_eq!(
            ok(&["scene", "switch", "s"]).command,
            Command::Action(Action::SceneSwitch {
                scene_id: SceneId::new("s"),
                preset_id: None,
            })
        );
        assert_eq!(
            ok(&["scene", "switch", "s", "--preset", "fade"]).command,
            Command::Action(Action::SceneSwitch {
                scene_id: SceneId::new("s"),
                preset_id: Some(TransitionPresetId::new("fade")),
            })
        );
        assert_eq!(
            ok(&["studio", "set-preview", "s"]).command,
            Command::Action(Action::StudioSetPreview {
                scene_id: SceneId::new("s"),
            })
        );
        assert_eq!(
            ok(&["studio", "get"]).command,
            Command::Query(Query::StudioGet)
        );
        assert!(err(&["studio"]).starts_with("wrong arguments for `"));
    }

    #[test]
    fn set_key_takes_a_key_file_and_never_the_key() {
        assert_eq!(
            ok(&["output", "set-key", "o"]).command,
            Command::SetKey {
                output_id: OutputId::new("o"),
                key_file: None,
            }
        );
        assert_eq!(
            ok(&["output", "set-key", "o", "--key-file", "/k"]).command,
            Command::SetKey {
                output_id: OutputId::new("o"),
                key_file: Some("/k".into()),
            }
        );
        assert_eq!(
            err(&["output", "set-key", "o", "--key-file"]),
            "`--key-file` needs a path"
        );
        for line in [
            &["output", "set-key", "o", "hunter2-key"][..],
            &["output", "set-key", "o", "--key", "hunter2-key"],
            &["output", "set-key", "o", "--key-file", "/k", "hunter2-key"],
        ] {
            let message = err(line);
            assert!(!message.contains("hunter2"), "{message}");
            assert!(message.contains("never from the command line"), "{message}");
        }
        assert!(err(&["output", "set-key"]).starts_with("wrong arguments for `"));
    }

    #[test]
    fn output_edits_take_an_rtmp_server_url() {
        let target = |url: &str| RtmpTarget::Generic { url: url.into() };
        let line = [
            "edit",
            "output.create",
            "yt",
            "rtmp://a.rtmp.youtube.com/live2",
            "output.set_target",
            "yt",
            "RTMPS://a.rtmps.youtube.com:443/live2",
            "output.delete",
            "yt",
        ];
        let Command::Edit(batch) = ok(&line).command else {
            panic!("not an edit");
        };
        assert_eq!(
            batch.edits,
            [
                Edit::OutputCreate {
                    output_id: OutputId::new("yt"),
                    kind: OutputKind::Rtmp {
                        target: target("rtmp://a.rtmp.youtube.com/live2"),
                    },
                },
                Edit::OutputSetTarget {
                    output_id: OutputId::new("yt"),
                    target: target("RTMPS://a.rtmps.youtube.com:443/live2"),
                },
                Edit::OutputDelete {
                    output_id: OutputId::new("yt"),
                },
            ]
        );
        for url in ["a.rtmp.youtube.com/live2", "srt://host/hunter2", "hunter2"] {
            let message = err(&["edit", "output.create", "o", url]);
            assert_eq!(
                message,
                "an output target must be an `rtmp://` or `rtmps://` server URL"
            );
        }
        assert_eq!(
            err(&["edit", "output.set_target", "o"]),
            "`output.set_target` takes 2 arguments; try `jstream-cli help`"
        );
    }

    #[test]
    fn edits_parse_in_sequence_into_one_batch() {
        let line = [
            "edit",
            "--expected-rev",
            "4",
            "--idempotency-key",
            "k1",
            "scene.create",
            "s1",
            "Main scene",
            "source.create",
            "cam",
            "Camera",
            "builtin:webcam",
            "source.create",
            "nest",
            "Nested",
            "scene:s1",
            "item.add",
            "s1",
            "i1",
            "cam",
            "item.set_visible",
            "i1",
            "false",
            "item.set_crop",
            "i1",
            "1",
            "2",
            "3",
            "4",
            "item.reorder",
            "i1",
            "0",
        ];
        let Command::Edit(batch) = ok(&line).command else {
            panic!("not an edit");
        };
        assert_eq!(
            batch,
            Batch {
                edits: vec![
                    Edit::SceneCreate {
                        scene_id: SceneId::new("s1"),
                        name: "Main scene".into(),
                    },
                    Edit::SourceCreate {
                        source_id: SourceId::new("cam"),
                        name: "Camera".into(),
                        kind: SourceKind::Builtin {
                            kind: "webcam".into()
                        },
                        settings: Default::default(),
                        duplicated_from: None,
                    },
                    Edit::SourceCreate {
                        source_id: SourceId::new("nest"),
                        name: "Nested".into(),
                        kind: SourceKind::Scene {
                            scene_id: SceneId::new("s1")
                        },
                        settings: Default::default(),
                        duplicated_from: None,
                    },
                    Edit::ItemAdd {
                        scene_id: SceneId::new("s1"),
                        item_id: ItemId::new("i1"),
                        source_id: SourceId::new("cam"),
                        transform: None,
                        geometry: None,
                    },
                    Edit::ItemSetVisible {
                        item_id: ItemId::new("i1"),
                        scene_id: None,
                        visible: false,
                    },
                    Edit::ItemSetCrop {
                        item_id: ItemId::new("i1"),
                        scene_id: None,
                        crop: Crop {
                            left: 1,
                            top: 2,
                            right: 3,
                            bottom: 4
                        },
                    },
                    Edit::ItemReorder {
                        item_id: ItemId::new("i1"),
                        index: 0,
                    },
                ],
                expected_rev: Some(Rev(4)),
                idempotency_key: Some(IdempotencyKey::new("k1")),
            }
        );
    }

    #[test]
    fn every_edit_name_parses() {
        let line = [
            "edit",
            "scene.rename",
            "s",
            "N",
            "scene.delete",
            "s",
            "scene.reorder",
            "s",
            "1",
            "source.duplicate",
            "a",
            "b",
            "B",
            "source.delete",
            "a",
            "item.remove",
            "i",
            "item.set_transform",
            "i",
            "10",
            "-5.5",
            "1",
            "2",
            "90",
            "scene.set_role",
            "s",
            "group",
            "item.move",
            "i",
            "s",
            "0",
            "item.set_geometry",
            "i",
            "960",
            "540",
            "1920",
            "1080",
            "0",
            "stretch",
            "canvas.set_size",
            "1280",
            "720",
            "item.set_source",
            "i",
            "b",
        ];
        let Command::Edit(batch) = ok(&line).command else {
            panic!("not an edit");
        };
        assert_eq!(
            batch.edits.last(),
            Some(&Edit::ItemSetSource {
                item_id: ItemId::new("i"),
                source_id: SourceId::new("b"),
            })
        );
        let names: Vec<&str> = batch.edits.iter().map(Edit::name).collect();
        assert_eq!(
            names,
            [
                "scene.rename",
                "scene.delete",
                "scene.reorder",
                "source.duplicate",
                "source.delete",
                "item.remove",
                "item.set_transform",
                "scene.set_role",
                "item.move",
                "item.set_geometry",
                "canvas.set_size",
                "item.set_source",
            ]
        );
        assert_eq!(
            batch.edits[7],
            Edit::SceneSetRole {
                scene_id: SceneId::new("s"),
                role: SceneRole::Group,
            }
        );
        assert_eq!(
            batch.edits[8],
            Edit::ItemMove {
                item_id: ItemId::new("i"),
                scene_id: SceneId::new("s"),
                index: 0,
            }
        );
        let Edit::ItemSetGeometry {
            scene_id: None,
            geometry,
            ..
        } = batch.edits[9]
        else {
            panic!("not a geometry edit: {:?}", batch.edits[9]);
        };
        assert_eq!(geometry.size.width.get(), 1920.0);
        assert_eq!(geometry.fit_mode, FitMode::Stretch);
        assert!(geometry.is_neutral_3d());
        assert_eq!(
            batch.edits[10],
            Edit::CanvasSetSize {
                width: 1280,
                height: 720,
            }
        );
        assert_eq!(batch.expected_rev, None);
        assert_eq!(batch.idempotency_key, None);
    }

    #[test]
    fn json_is_found_where_parse_finds_it() {
        use super::json_requested;
        let yes = |line: &[&str]| {
            assert!(json_requested(&args(line)), "{line:?}");
            assert!(parse(&args(line)).map_or(true, |inv| inv.json), "{line:?}");
        };
        yes(&["--json", "hello"]);
        // Regression (review round 1, B1): `--mock-output`'s value is not a command.
        yes(&[
            "--mock",
            "--mock-output",
            "out_1",
            "--json",
            "output",
            "stop",
            "out_1",
        ]);
        yes(&["--mock", "--mock-output", "out_1", "--json", "scene"]);
        let no = |line: &[&str]| assert!(!json_requested(&args(line)), "{line:?}");
        no(&["hello", "--json"]);
        no(&["--mock-output", "--json", "hello"]);
        no(&["--verbose", "--json", "hello"]);
        no(&["--help", "--json"]);
        yes(&[
            "--url",
            "ws://127.0.0.1:1",
            "--token-file",
            "t",
            "--json",
            "hello",
        ]);
        no(&["--url", "--json", "hello"]);
    }

    #[test]
    fn remote_options_name_the_engine_and_token() {
        assert_eq!(
            ok(&[
                "--url",
                "ws://127.0.0.1:4455",
                "--token-file",
                "/t",
                "hello"
            ])
            .connection,
            Connection::Remote {
                url: Some("ws://127.0.0.1:4455".into()),
                token_file: Some("/t".into()),
            }
        );
        assert!(err(&["--url"]).contains("`--url` needs a value"));
        assert!(err(&["--token-file", "a", "--token-file", "b", "hello"]).contains("twice"));
        assert!(err(&["--mock", "--url", "ws://x:1", "hello"]).contains("can't be combined"));
    }

    #[test]
    fn mcp_takes_the_connection_options_and_no_arguments() {
        assert_eq!(
            ok(&["--url", "ws://127.0.0.1:4455", "--token-file", "/t", "mcp"]),
            Invocation {
                json: false,
                connection: Connection::Remote {
                    url: Some("ws://127.0.0.1:4455".into()),
                    token_file: Some("/t".into()),
                },
                command: Command::Mcp,
            }
        );
        assert_eq!(
            ok(&["--mock", "--mock-output", "o", "mcp"]),
            Invocation {
                json: false,
                connection: Connection::Mock {
                    outputs: vec![OutputId::new("o")]
                },
                command: Command::Mcp,
            }
        );
        assert_eq!(
            err(&["mcp", "--stdio"]),
            "wrong arguments for `mcp`; try `jstream-cli help`"
        );
    }

    #[test]
    fn events_takes_an_optional_count() {
        assert_eq!(ok(&["events"]).command, Command::Events { count: None });
        assert_eq!(
            ok(&["events", "--count", "3"]).command,
            Command::Events { count: Some(3) }
        );
    }

    #[test]
    fn mistakes_are_usage_errors_that_say_what_is_wrong() {
        assert_eq!(err(&[]), "missing command; try `jstream-cli help`");
        assert_eq!(err(&["--verbose", "hello"]), "unknown option `--verbose`");
        assert_eq!(
            err(&["--mock-output"]),
            "`--mock-output` needs an output ID"
        );
        assert_eq!(
            err(&["--mock-output", "o", "hello"]),
            "`--mock-output` needs `--mock`"
        );
        assert_eq!(
            err(&["frobnicate"]),
            "unknown command `frobnicate`; try `jstream-cli help`"
        );
        assert_eq!(
            err(&["scene", "explode"]),
            "wrong arguments for `scene`; try `jstream-cli help`"
        );
        assert_eq!(
            err(&["edit"]),
            "`edit` needs at least one edit; try `jstream-cli help`"
        );
        assert_eq!(
            err(&["edit", "--expected-rev"]),
            "`--expected-rev` needs a value"
        );
        assert_eq!(
            err(&["edit", "--expected-rev", "-1", "scene.delete", "s"]),
            "`--expected-rev` must be a non-negative integer, not `-1`"
        );
        assert_eq!(
            err(&["edit", "scene.explode", "s"]),
            "unknown edit `scene.explode`; edits are protocol command names such as `scene.create`"
        );
        assert_eq!(
            err(&["edit", "scene.create", "s"]),
            "`scene.create` takes 2 arguments; try `jstream-cli help`"
        );
        assert_eq!(
            err(&["edit", "item.set_visible", "i", "yes"]),
            "`item.set_visible` takes `true` or `false`, not `yes`"
        );
        assert_eq!(
            err(&["edit", "source.create", "a", "A", "webcam"]),
            "source kind `webcam` must be `builtin:<kind>` or `scene:<scene-id>`"
        );
        assert_eq!(
            err(&["edit", "item.set_transform", "i", "inf", "0", "1", "1", "0"]),
            "`x` must be a finite number, not `inf`"
        );
        assert_eq!(
            err(&["edit", "scene.set_role", "s", "folder"]),
            "`scene.set_role` takes `scene` or `group`, not `folder`"
        );
        assert_eq!(
            err(&[
                "edit",
                "item.set_geometry",
                "i",
                "0",
                "0",
                "1",
                "1",
                "0",
                "zoom"
            ]),
            "`item.set_geometry` takes `stretch`, `fit` or `fill`, not `zoom`"
        );
        assert_eq!(
            err(&[
                "edit",
                "item.set_geometry",
                "i",
                "0",
                "NaN",
                "1",
                "1",
                "0",
                "fit"
            ]),
            "`center-y` must be a finite number, not `NaN`"
        );
        assert_eq!(
            err(&["edit", "item.move", "i", "s", "-1"]),
            "`index` must be a non-negative integer, not `-1`"
        );
        assert_eq!(
            err(&["edit", "canvas.set_size", "1920", "1080.5"]),
            "`height` must be a non-negative integer, not `1080.5`"
        );
        assert_eq!(
            err(&["events", "--count", "many"]),
            "`--count` must be a non-negative integer, not `many`"
        );
        assert_eq!(
            err(&["events", "--follow"]),
            "unexpected `--follow` after `events`"
        );
    }
}
