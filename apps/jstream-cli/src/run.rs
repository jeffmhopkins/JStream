//! Running a parsed command against a `Control` session and writing its output.
//!
//! Everything here writes through the `Write` handles it is given, so tests capture the
//! output (golden strings) and the binary passes locked stdout and stderr. The session
//! comes from a [`Connector`]: the WebSocket client in the binary, or a `MockEngine`.
//! `mcp` and `output set-key` also read the input they are given (stdin in the binary).

use crate::args::{self, Command, Connection, Invocation, USAGE};
use crate::block::{block_on, next_event};
use crate::exit::{self, CliError};
use crate::mcp;
use jstream_control::protocol::{
    Action, ActionResult, Batch, Call, CallResult, Canvas, Edit, Event, Hello, HotkeyBinding,
    ItemGeometry, ItemInfo, OutputInfo, OutputKind, PROTOCOL_VERSION, Query, QueryResult, Response,
    RtmpTarget, SecretString, SourceId, SourceInfo, SourceKind, Welcome,
};
use jstream_control::{Control, ControlError, EventStream, WsConfig, WsControl};
use serde_json::json;
use std::io::{self, BufRead, Read, Write};

/// The name this client gives in `hello`, shown in the engine's audit log.
pub const CLIENT_NAME: &str = "jstream-cli";

/// Opens a `Control` session for a [`Connection`].
pub type Connector<'a> = &'a dyn Fn(&Connection) -> Result<Box<dyn Control>, CliError>;

/// The real connector: the engine's WebSocket API (`--url`, with the token read from the
/// token file), or a fresh mock engine for `--mock` (feature `mock`).
pub fn connect(connection: &Connection) -> Result<Box<dyn Control>, CliError> {
    match connection {
        #[cfg(feature = "mock")]
        Connection::Mock { outputs } => {
            let engine = outputs
                .iter()
                .fold(jstream_control::MockEngine::new(), |engine, id| {
                    engine.with_output(id.clone())
                });
            Ok(Box::new(engine.connect()))
        }
        #[cfg(not(feature = "mock"))]
        Connection::Mock { .. } => Err(CliError::Usage(
            "`--mock` needs a build with the `mock` feature \
             (`cargo build -p jstream-cli --features mock`)"
                .into(),
        )),
        Connection::Remote { url, token_file } => {
            let url = url.as_deref().ok_or_else(|| {
                CliError::Usage(
                    "no engine to talk to: pass `--url ws://127.0.0.1:<port>` \
                     (or `--mock`)"
                        .into(),
                )
            })?;
            let token = read_token(
                token_file
                    .clone()
                    .or_else(|| std::env::var(TOKEN_FILE_ENV).ok()),
            )?;
            let control = block_on(WsControl::connect(WsConfig::new(url, token)))?;
            Ok(Box::new(control))
        }
    }
}

/// The environment variable naming the token file when `--token-file` is not given.
pub const TOKEN_FILE_ENV: &str = "JSTREAM_TOKEN_FILE";

/// Reads the control token from `path`: the file's text without surrounding whitespace.
/// The token is never taken from the command line, which other users can see in the
/// process list (ADR-0006: the token lives in a user-only file).
fn read_token(path: Option<String>) -> Result<String, CliError> {
    let path = path.ok_or_else(|| {
        CliError::Usage(format!(
            "no control token: pass `--token-file <path>` or set {TOKEN_FILE_ENV}"
        ))
    })?;
    let text = std::fs::read_to_string(&path).map_err(|error| {
        CliError::Usage(format!("could not read the token file `{path}`: {error}"))
    })?;
    let token = text.trim();
    if token.is_empty() {
        return Err(CliError::Usage(format!("the token file `{path}` is empty")));
    }
    Ok(token.to_owned())
}

/// The most `output set-key` reads, in bytes. The engine takes keys of up to 1024 bytes;
/// the slack covers surrounding whitespace. More than this is not a stream key (a file
/// piped in by mistake, say), so it is refused before it is buffered or sent.
const MAX_KEY_INPUT: u64 = 4096;

/// Reads the stream key for `output set-key`: the file's text, or else all of `input`
/// (stdin), without a leading byte-order mark (Windows PowerShell 5.1 writes one on a
/// pipe, and Notepad's "UTF-8 with BOM" starts a file with one, #438) and without
/// surrounding whitespace (a key has none, and a file or a pipe usually ends with a
/// newline). At most [`MAX_KEY_INPUT`] bytes are read. A key with a control or
/// non-ASCII character is refused: no streaming service issues one. No message repeats
/// any of what was read.
fn read_key(key_file: Option<&str>, input: &mut dyn BufRead) -> Result<SecretString, CliError> {
    let mut text = String::new();
    let read = match key_file {
        Some(path) => std::fs::File::open(path)
            .and_then(|file| file.take(MAX_KEY_INPUT + 1).read_to_string(&mut text))
            .map_err(|error| {
                CliError::Usage(format!("could not read the key file `{path}`: {error}"))
            })?,
        None => input
            .take(MAX_KEY_INPUT + 1)
            .read_to_string(&mut text)
            .map_err(CliError::Read)?,
    };
    let source = match key_file {
        Some(path) => format!("the key file `{path}`"),
        None => "stdin".into(),
    };
    if !u64::try_from(read).is_ok_and(|read| read <= MAX_KEY_INPUT) {
        return Err(CliError::Usage(format!(
            "{source} holds more than {MAX_KEY_INPUT} bytes, which is not a stream key"
        )));
    }
    let key = text.strip_prefix('\u{FEFF}').unwrap_or(&text).trim();
    if key.is_empty() {
        return Err(CliError::Usage(match key_file {
            Some(path) => format!("the key file `{path}` is empty"),
            None => "no stream key on stdin: pipe it in, or pass `--key-file <path>`".into(),
        }));
    }
    if key.chars().any(|c| c.is_control() || !c.is_ascii()) {
        return Err(CliError::Usage(format!(
            "{source} holds a stream key with a control or non-ASCII character, which no \
             streaming service accepts (the key is not shown)"
        )));
    }
    Ok(SecretString::new(key))
}

/// Parses `args`, runs the command, writes its output to `out` and any error to `err`,
/// and returns the process exit code. Only `mcp` and `output set-key` read `input`.
pub fn run(
    args: &[String],
    connector: Connector<'_>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let (result, json) = match args::parse(args) {
        Ok(invocation) => (execute(&invocation, connector, input, out), invocation.json),
        // A usage error is reported as JSON too if the global options asked for it.
        Err(error) => (Err(error), args::json_requested(args)),
    };
    match result {
        Ok(()) => exit::OK,
        Err(error) => {
            report(&error, json, err);
            error.exit_code()
        }
    }
}

/// Runs one parsed invocation.
pub fn execute(
    invocation: &Invocation,
    connector: Connector<'_>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let json = invocation.json;
    match &invocation.command {
        // The MCP server sends `hello` itself, with the MCP client's name.
        Command::Mcp => return mcp::serve(&invocation.connection, connector, input, out),
        Command::Help => {
            out.write_all(USAGE.as_bytes())?;
            return Ok(());
        }
        Command::Version => {
            let version = env!("CARGO_PKG_VERSION");
            if json {
                line(out, &json!({"cli": version, "protocol": PROTOCOL_VERSION}))?;
            } else {
                writeln!(out, "jstream-cli {version}, protocol {PROTOCOL_VERSION}")?;
            }
            return Ok(());
        }
        _ => {}
    }
    // A stream key is read before connecting, so a missing one fails without a session.
    let action = match &invocation.command {
        Command::Action(action) => Some(action.clone()),
        Command::SetKey {
            output_id,
            key_file,
        } => Some(Action::OutputSetKey {
            output_id: output_id.clone(),
            key: read_key(key_file.as_deref(), input)?,
        }),
        _ => None,
    };
    let control = connector(&invocation.connection)?;
    let welcome = block_on(control.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: CLIENT_NAME.into(),
    }))?;
    if let Some(action) = action {
        let response = block_on(control.call(Call::Action {
            action: action.clone(),
        }))?;
        return if json {
            line(out, &response)
        } else {
            write_action(out, &action, &response)
        };
    }
    match &invocation.command {
        Command::Hello => write_welcome(out, &welcome, json),
        Command::Query(query) => {
            let call = Call::Query {
                query: query.clone(),
            };
            write_response(out, &block_on(control.call(call))?, json)
        }
        Command::Edit(batch) => {
            let mut batch = batch.clone();
            fit_captures(&mut batch, || state(&*control))?;
            let call = Call::Batch(batch);
            write_response(out, &block_on(control.call(call))?, json)
        }
        Command::Events { count } => stream_events(control.events(), *count, json, out),
        Command::Help
        | Command::Version
        | Command::Mcp
        | Command::Action(_)
        | Command::SetKey { .. } => Ok(()),
    }
}

/// The source kinds whose frames come at their own size (a screen, a window, a camera),
/// so an item showing one at the legacy 1:1 placement shows only its top-left corner.
const CAPTURE_KINDS: &[&str] = &["display", "window", "webcam"];

/// Gives each `item.add` in `batch` that names no placement and shows a capture source a
/// `geometry` covering the canvas with `fit`, so the whole capture is in frame.
///
/// The protocol's own default for an omitted placement stays the legacy 1:1 one
/// (ADR-0022); this is the CLI choosing a placement for the user. The canvas is the
/// batch's latest `canvas.set_size` before the item, and a source created earlier in the
/// batch gives its kind; anything else comes from `state` (`state.get`), called at most
/// once and only when an item needs it.
fn fit_captures(
    batch: &mut Batch,
    state: impl FnOnce() -> Result<(Canvas, Vec<SourceInfo>), CliError>,
) -> Result<(), CliError> {
    let mut state = Some(state);
    let mut current: Option<(Canvas, Vec<SourceInfo>)> = None;
    let mut canvas: Option<Canvas> = None;
    let mut created: Vec<(SourceId, SourceKind)> = Vec::new();
    for edit in &mut batch.edits {
        match edit {
            Edit::CanvasSetSize { width, height } => {
                canvas = Some(Canvas {
                    width: *width,
                    height: *height,
                });
            }
            Edit::SourceCreate {
                source_id, kind, ..
            } => created.push((source_id.clone(), kind.clone())),
            Edit::ItemAdd {
                source_id,
                transform: None,
                geometry: geometry @ None,
                ..
            } => {
                let known = created
                    .iter()
                    .rev()
                    .find(|(id, _)| id == source_id)
                    .map(|(_, kind)| kind.clone());
                if known.as_ref().is_some_and(|kind| !is_capture(kind)) {
                    continue;
                }
                if (known.is_none() || canvas.is_none())
                    && let Some(read) = state.take()
                {
                    current = Some(read()?);
                }
                let kind = known.or_else(|| {
                    current.as_ref().and_then(|(_, sources)| {
                        sources
                            .iter()
                            .find(|source| &source.source_id == source_id)
                            .map(|source| source.kind.clone())
                    })
                });
                // An unknown source is left for the engine to reject.
                if !kind.as_ref().is_some_and(is_capture) {
                    continue;
                }
                let Some(size) = canvas.or(current.as_ref().map(|(canvas, _)| *canvas)) else {
                    continue;
                };
                // A zero canvas is left for the engine to reject too.
                *geometry = ItemGeometry::covering_canvas(size.width, size.height).ok();
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_capture(kind: &SourceKind) -> bool {
    matches!(kind, SourceKind::Builtin { kind } if CAPTURE_KINDS.contains(&kind.as_str()))
}

/// The canvas and sources, from `state.get`.
fn state(control: &dyn Control) -> Result<(Canvas, Vec<SourceInfo>), CliError> {
    let response = block_on(control.call(Call::Query {
        query: Query::StateGet,
    }))?;
    match response.result {
        CallResult::Query {
            result: QueryResult::StateGet {
                canvas, sources, ..
            },
        } => Ok((canvas, sources)),
        _ => Err(ControlError::Disconnected {
            reason: "the engine answered `state.get` with another result".into(),
        }
        .into()),
    }
}

/// Prints events as they arrive, flushing after each, until `count` have been printed.
/// A stream that ends first means the engine went away: `Disconnected`.
fn stream_events(
    mut events: EventStream,
    count: Option<u64>,
    json: bool,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let mut printed = 0;
    while count.is_none_or(|n| printed < n) {
        let Some(event) = next_event(&mut events) else {
            return Err(ControlError::Disconnected {
                reason: "the event stream ended".into(),
            }
            .into());
        };
        if write_event(out, &event, json)? {
            printed += 1;
            out.flush()?;
        }
    }
    Ok(())
}

/// Writes `value` as one line of JSON.
fn line(out: &mut dyn Write, value: &impl serde::Serialize) -> Result<(), CliError> {
    serde_json::to_writer(&mut *out, value).map_err(io::Error::from)?;
    writeln!(out)?;
    Ok(())
}

/// The wire spelling of a unit enum value (`active`), for human output.
fn wire_name(value: &impl serde::Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(other) => other.to_string(),
        Err(_) => "?".into(),
    }
}

fn write_welcome(out: &mut dyn Write, welcome: &Welcome, json: bool) -> Result<(), CliError> {
    if json {
        return line(out, welcome);
    }
    writeln!(
        out,
        "connected: protocol {}, client {}, rev {}",
        welcome.protocol, welcome.client_id, welcome.rev
    )?;
    Ok(())
}

fn write_action(out: &mut dyn Write, action: &Action, response: &Response) -> Result<(), CliError> {
    let id = &response.id;
    match action {
        Action::OutputStart { output_id }
        | Action::OutputStop { output_id }
        | Action::OutputSetKey { output_id, .. }
        | Action::OutputClearKey { output_id } => {
            writeln!(out, "{} accepted for {output_id} ({id})", action.name())?;
        }
        Action::SceneSwitch { scene_id, .. } | Action::StudioSetPreview { scene_id } => {
            writeln!(out, "{} accepted for {scene_id} ({id})", action.name())?;
        }
        Action::DeviceRevoke { device_id } => {
            writeln!(out, "device {device_id} revoked ({id})")?;
        }
        Action::PairingStart => match &response.result {
            CallResult::Action {
                result:
                    Some(ActionResult::PairingStart {
                        code,
                        expires_in_secs,
                    }),
            } => {
                // The code is the answer: printed for the user to type on the device,
                // never logged (R-SEC-001).
                writeln!(out, "pairing code: {}", code.expose())?;
                writeln!(
                    out,
                    "enter it on the device within {expires_in_secs} s; it works once"
                )?;
            }
            _ => writeln!(
                out,
                "{} accepted, but no code came back ({id})",
                action.name()
            )?,
        },
        other => writeln!(out, "{} accepted ({id})", other.name())?,
    }
    Ok(())
}

fn write_response(out: &mut dyn Write, response: &Response, json: bool) -> Result<(), CliError> {
    if json {
        return line(out, response);
    }
    match &response.result {
        CallResult::Batch {
            rev,
            removed_hotkeys,
            dropped_overrides,
        } => {
            writeln!(out, "applied: rev {rev} ({})", response.id)?;
            // ADR-0040 §3: a delete takes the hotkeys naming its target with it.
            for binding in removed_hotkeys {
                writeln!(out, "removed hotkey {}", binding.chord)?;
            }
            // ADR-0040 §1: an item leaving its scene takes its keyframe overrides.
            for dropped in dropped_overrides {
                writeln!(
                    out,
                    "dropped the overrides of item {} in scene {}",
                    dropped.dropped.item_id, dropped.scene_id
                )?;
            }
        }
        CallResult::Action { result: None } => writeln!(out, "accepted ({})", response.id)?,
        CallResult::Query { result } => write_query(out, result)?,
        _ => line(out, &response.result)?,
    }
    Ok(())
}

fn write_query(out: &mut dyn Write, result: &QueryResult) -> Result<(), CliError> {
    match result {
        QueryResult::SceneList { rev, scenes } => {
            writeln!(out, "rev {rev}")?;
            if scenes.is_empty() {
                writeln!(out, "no scenes")?;
                return Ok(());
            }
            let rows = scenes.iter().map(|scene| {
                vec![
                    scene.scene_id.to_string(),
                    scene.name.clone(),
                    wire_name(&scene.role),
                    scene.item_count.to_string(),
                ]
            });
            table(out, "", &["ID", "NAME", "ROLE", "ITEMS"], rows.collect())?;
        }
        QueryResult::OutputStatus {
            output_id,
            state,
            key_set,
            encoder,
            cleartext,
            ..
        } => {
            let key = if *key_set { "set" } else { "not set" };
            write!(out, "{output_id}: {}, stream key {key}", wire_name(state))?;
            match encoder.as_ref().map(|encoder| &encoder.shared_with) {
                Some(_) if encoder.as_ref().is_some_and(|encoder| encoder.dedicated) => {
                    write!(out, ", own encoder (own video chain)")?;
                }
                Some(shared) if shared.is_empty() => write!(out, ", own encoder")?,
                Some(shared) => {
                    let names: Vec<_> = shared.iter().map(ToString::to_string).collect();
                    write!(out, ", encoder shared with {}", names.join(", "))?;
                }
                None => {}
            }
            if *cleartext {
                write!(
                    out,
                    "; warning: plain rtmp://, the stream key is sent unencrypted"
                )?;
            }
            writeln!(out)?;
        }
        QueryResult::OutputList { rev, outputs } => {
            writeln!(out, "rev {rev}")?;
            write_outputs(out, "", outputs)?;
        }
        QueryResult::DeviceList { devices } => {
            if devices.is_empty() {
                writeln!(out, "no paired devices")?;
                return Ok(());
            }
            let rows = devices.iter().map(|device| {
                vec![
                    device.device_id.to_string(),
                    device.name.clone(),
                    device.paired_at.to_string(),
                    device.last_used_at.to_string(),
                ]
            });
            table(
                out,
                "",
                &["ID", "NAME", "PAIRED (UNIX S)", "LAST USED (UNIX S)"],
                rows.collect(),
            )?;
        }
        QueryResult::StudioGet {
            program_scene,
            preview_scene,
        } => {
            for (label, scene) in [("program", program_scene), ("preview", preview_scene)] {
                match scene {
                    Some(scene_id) => writeln!(out, "{label}: {scene_id}")?,
                    None => writeln!(out, "{label}: none (no scenes)")?,
                }
            }
        }
        QueryResult::SourceList { rev, sources } => {
            writeln!(out, "rev {rev}")?;
            write_sources(out, "", sources)?;
        }
        QueryResult::CaptureTargets { targets } => {
            if targets.is_empty() {
                writeln!(out, "nothing to capture")?;
            }
            for target in targets {
                write!(
                    out,
                    "{} {} ({})",
                    wire_name(&target.kind),
                    target.target,
                    target.name
                )?;
                if let Some(size) = target.size {
                    write!(out, " {}x{}", size.width, size.height)?;
                }
                if target.primary {
                    write!(out, ", primary")?;
                }
                writeln!(out)?;
            }
        }
        QueryResult::ItemList {
            rev,
            scene_id,
            items,
        } => {
            writeln!(out, "rev {rev}, scene {scene_id}")?;
            write_items(out, "", items)?;
        }
        QueryResult::VideoSnapshot {
            rev,
            width,
            height,
            png,
        } => {
            writeln!(
                out,
                "rev {rev}, {width}x{height} png ({} base64 chars)",
                png.len()
            )?;
        }
        QueryResult::StateGet {
            rev,
            sources,
            scenes,
            outputs,
            hotkeys,
            ..
        } => {
            writeln!(out, "rev {rev}")?;
            writeln!(out, "sources:")?;
            write_sources(out, "  ", sources)?;
            if scenes.is_empty() {
                writeln!(out, "no scenes")?;
            }
            for scene in scenes {
                let role = wire_name(&scene.role);
                writeln!(out, "{role} {} ({}):", scene.scene_id, scene.name)?;
                write_items(out, "  ", &scene.items)?;
            }
            writeln!(out, "outputs:")?;
            write_outputs(out, "  ", outputs)?;
            if !hotkeys.is_empty() {
                writeln!(out, "hotkeys:")?;
                write_hotkeys(out, "  ", hotkeys)?;
            }
        }
        other => line(out, other)?,
    }
    Ok(())
}

/// Hotkey bindings as a table, each action as its name and the IDs it names (the spelling
/// `hotkey.set` takes, plus a `scene.switch`'s preset after its scene). An action this
/// CLI doesn't know prints as JSON.
fn write_hotkeys(
    out: &mut dyn Write,
    indent: &str,
    hotkeys: &[HotkeyBinding],
) -> Result<(), CliError> {
    let rows = hotkeys.iter().map(|binding| {
        let action = match binding.action() {
            Some(action) => {
                let ids: Vec<&str> = action
                    .named_objects()
                    .into_iter()
                    .map(|(_, id)| id)
                    .collect();
                format!("{} {}", action.name(), ids.join(" "))
            }
            None => binding.action.to_string(),
        };
        vec![binding.chord.to_string(), action]
    });
    table(out, indent, &["CHORD", "ACTION"], rows.collect())
}

/// Sources as a table, in the `source.create` argument spelling of their kind.
fn write_sources(
    out: &mut dyn Write,
    indent: &str,
    sources: &[SourceInfo],
) -> Result<(), CliError> {
    if sources.is_empty() {
        writeln!(out, "{indent}no sources")?;
        return Ok(());
    }
    let rows = sources.iter().map(|source| {
        let kind = match &source.kind {
            SourceKind::Builtin { kind } => format!("builtin:{kind}"),
            SourceKind::Scene { scene_id } => format!("scene:{scene_id}"),
            other => serde_json::to_string(other).unwrap_or_else(|_| "?".into()),
        };
        vec![source.source_id.to_string(), source.name.clone(), kind]
    });
    table(out, indent, &["ID", "NAME", "KIND"], rows.collect())
}

/// Outputs as a table, with the target in the spelling `output.create` takes (the RTMP
/// server URL). A kind or target this CLI doesn't know prints as JSON.
fn write_outputs(
    out: &mut dyn Write,
    indent: &str,
    outputs: &[OutputInfo],
) -> Result<(), CliError> {
    if outputs.is_empty() {
        writeln!(out, "{indent}no outputs")?;
        return Ok(());
    }
    let rows = outputs.iter().map(|output| {
        let (kind, target) = match &output.kind {
            OutputKind::Rtmp {
                target: RtmpTarget::Generic { url },
            } => ("rtmp".to_owned(), url.clone()),
            other => (
                serde_json::to_value(other)
                    .ok()
                    .and_then(|value| value.get("type").map(wire_name))
                    .unwrap_or_else(|| "?".into()),
                serde_json::to_string(other).unwrap_or_else(|_| "?".into()),
            ),
        };
        vec![output.output_id.to_string(), kind, target]
    });
    table(out, indent, &["ID", "KIND", "TARGET"], rows.collect())
}

/// Items as a table, bottom first, with each property in the argument order of the edit
/// that sets it.
fn write_items(out: &mut dyn Write, indent: &str, items: &[ItemInfo]) -> Result<(), CliError> {
    if items.is_empty() {
        writeln!(out, "{indent}no items")?;
        return Ok(());
    }
    let rows = items.iter().map(|item| {
        let t = &item.transform;
        let c = &item.crop;
        vec![
            item.item_id.to_string(),
            item.source_id.to_string(),
            item.visible.to_string(),
            format!("{},{}", t.position.x, t.position.y),
            format!("{},{}", t.scale.x, t.scale.y),
            t.rotation_deg.to_string(),
            format!("{},{},{},{}", c.left, c.top, c.right, c.bottom),
        ]
    });
    table(
        out,
        indent,
        &[
            "ID", "SOURCE", "VISIBLE", "POSITION", "SCALE", "ROTATION", "CROP",
        ],
        rows.collect(),
    )
}

/// Writes a left-aligned table: columns two spaces apart, each as wide as its widest
/// cell (counted in characters), and the last one unpadded, so no line ends in spaces.
fn table(
    out: &mut dyn Write,
    indent: &str,
    headers: &[&str],
    rows: Vec<Vec<String>>,
) -> Result<(), CliError> {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(column, header)| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.chars().count())
                .chain([header.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let header: Vec<String> = headers.iter().map(|h| (*h).to_owned()).collect();
    for row in std::iter::once(&header).chain(&rows) {
        let last = row.len().saturating_sub(1);
        let mut text = String::from(indent);
        for (column, cell) in row.iter().enumerate() {
            if column == last {
                text.push_str(cell);
            } else {
                let width = widths.get(column).copied().unwrap_or(0);
                text.push_str(&format!("{cell:width$}  "));
            }
        }
        writeln!(out, "{text}")?;
    }
    Ok(())
}

/// Writes one event. Events this protocol version doesn't know are skipped, as the
/// protocol asks; returns whether anything was written.
fn write_event(out: &mut dyn Write, event: &Event, json: bool) -> Result<bool, CliError> {
    if event.name().is_none() {
        return Ok(false);
    }
    if json {
        line(out, event)?;
        return Ok(true);
    }
    match event {
        Event::StateChanged {
            rev,
            client_id,
            request_id,
            cause,
        } => {
            write!(out, "state.changed rev {rev} by {client_id}")?;
            if let Some(request_id) = request_id {
                write!(out, " ({request_id})")?;
            }
            if let Some(cause) = cause {
                write!(out, " {cause}")?;
            }
            writeln!(out)?;
        }
        Event::OutputStateChanged { output_id, state } => {
            writeln!(out, "output.state_changed {output_id} {}", wire_name(state))?;
        }
        Event::StudioChanged {
            program_scene,
            preview_scene,
            transition,
            client_id,
            request_id,
        } => {
            write!(out, "studio.changed")?;
            for (label, scene) in [("program", program_scene), ("preview", preview_scene)] {
                match scene {
                    Some(scene_id) => write!(out, " {label} {scene_id}")?,
                    None => write!(out, " {label} none")?,
                }
            }
            if let Some(transition) = transition {
                write!(
                    out,
                    " transition {} {}ms",
                    transition.preset_id, transition.duration_ms
                )?;
            }
            write!(out, " by {client_id}")?;
            if let Some(request_id) = request_id {
                write!(out, " ({request_id})")?;
            }
            writeln!(out)?;
        }
        Event::EventsDropped { count } => {
            writeln!(out, "events.dropped {count}")?;
        }
        Event::StatsTick { seq, drops, .. } => {
            writeln!(
                out,
                "stats.tick seq {seq} capture_frames {} audio_frames {} render_ticks {} \
                 encode_frames {} encode_blocks {} output_packets {}",
                drops.capture_frames,
                drops.audio_frames,
                drops.render_ticks,
                drops.encode_frames,
                drops.encode_blocks,
                drops.output_packets,
            )?;
        }
        Event::LiveChanged {
            gesture_id,
            client_id,
            values,
        } => {
            let names: Vec<&str> = values.iter().map(Edit::name).collect();
            writeln!(
                out,
                "live.changed {gesture_id} by {client_id}: {}",
                names.join(", ")
            )?;
        }
        Event::LiveEnded {
            gesture_id,
            client_id,
            outcome,
            rev,
        } => {
            write!(
                out,
                "live.ended {gesture_id} by {client_id} {}",
                outcome.name()
            )?;
            if let Some(rev) = rev {
                write!(out, " rev {rev}")?;
            }
            writeln!(out)?;
        }
        other => line(out, other)?,
    }
    Ok(true)
}

/// Writes `error` to `err`: `error: …` for people, or a JSON object with a `code` for
/// machines. A closed output pipe is not reported: nobody is reading.
fn report(error: &CliError, json: bool, err: &mut dyn Write) {
    if let CliError::Io(e) = error
        && e.kind() == io::ErrorKind::BrokenPipe
    {
        return;
    }
    // Reporting is best effort: if stderr is gone too, the exit code still says it.
    let _ = if json {
        let body = match error {
            CliError::Control(ControlError::Protocol(wire)) => json!({ "error": wire }),
            CliError::Control(_) => {
                json!({"error": {"code": "disconnected", "message": error.to_string()}})
            }
            CliError::Usage(message) => json!({"error": {"code": "usage", "message": message}}),
            CliError::Io(e) => json!({"error": {"code": "io", "message": e.to_string()}}),
            // Says which side failed, as `io` covers both.
            CliError::Read(_) => {
                json!({"error": {"code": "io", "message": error.to_string()}})
            }
        };
        writeln!(err, "{body}")
    } else {
        writeln!(err, "error: {error}")
    };
}

/// Golden-output tests against the mock engine, in their own file for size.
#[cfg(test)]
mod tests;
