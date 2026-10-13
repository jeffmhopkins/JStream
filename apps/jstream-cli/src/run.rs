//! Running a parsed command against a `Control` session and writing its output.
//!
//! Everything here writes through the `Write` handles it is given, so tests capture the
//! output (golden strings) and the binary passes locked stdout and stderr. The session
//! comes from a [`Connector`], which is the seam the WebSocket client plugs into later.

use crate::args::{self, Command, Connection, Invocation, USAGE};
use crate::block::{block_on, next_event};
use crate::exit::{self, CliError};
use jstream_control::protocol::{
    Action, Call, CallResult, Event, Hello, PROTOCOL_VERSION, QueryResult, Response, Welcome,
};
use jstream_control::{Control, ControlError, EventStream};
use serde_json::json;
use std::io::{self, Write};

/// The name this client gives in `hello`, shown in the engine's audit log.
pub const CLIENT_NAME: &str = "jstream-cli";

/// Opens a `Control` session for a [`Connection`].
pub type Connector<'a> = &'a dyn Fn(&Connection) -> Result<Box<dyn Control>, CliError>;

/// The real connector: a fresh mock engine for `--mock` (feature `mock`); no remote
/// transport yet.
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
        Connection::Remote => Err(ControlError::Disconnected {
            reason: "the WebSocket client is not built yet; \
                     pass `--mock` to use the in-memory mock engine"
                .into(),
        }
        .into()),
    }
}

/// Parses `args`, runs the command, writes its output to `out` and any error to `err`,
/// and returns the process exit code.
pub fn run(
    args: &[String],
    connector: Connector<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let (result, json) = match args::parse(args) {
        Ok(invocation) => (execute(&invocation, connector, out), invocation.json),
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
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let json = invocation.json;
    match &invocation.command {
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
    let control = connector(&invocation.connection)?;
    let welcome = block_on(control.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: CLIENT_NAME.into(),
    }))?;
    match &invocation.command {
        Command::Hello => write_welcome(out, &welcome, json),
        Command::Query(query) => {
            let call = Call::Query {
                query: query.clone(),
            };
            write_response(out, &block_on(control.call(call))?, json)
        }
        Command::Action(action) => {
            let call = Call::Action {
                action: action.clone(),
            };
            let response = block_on(control.call(call))?;
            if json {
                line(out, &response)
            } else {
                write_action(out, action, &response)
            }
        }
        Command::Edit(batch) => {
            let call = Call::Batch(batch.clone());
            write_response(out, &block_on(control.call(call))?, json)
        }
        Command::Events { count } => stream_events(control.events(), *count, json, out),
        Command::Help | Command::Version => Ok(()),
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
        Action::OutputStart { output_id } | Action::OutputStop { output_id } => {
            writeln!(out, "{} accepted for {output_id} ({id})", action.name())?;
        }
        other => writeln!(out, "{} accepted ({id})", other.name())?,
    }
    Ok(())
}

fn write_response(out: &mut dyn Write, response: &Response, json: bool) -> Result<(), CliError> {
    if json {
        return line(out, response);
    }
    match &response.result {
        CallResult::Batch { rev } => writeln!(out, "applied: rev {rev} ({})", response.id)?,
        CallResult::Action {} => writeln!(out, "accepted ({})", response.id)?,
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
            let id_width = scenes
                .iter()
                .map(|s| s.scene_id.as_str().chars().count())
                .chain([2])
                .max()
                .unwrap_or(2);
            let name_width = scenes
                .iter()
                .map(|s| s.name.chars().count())
                .chain([4])
                .max()
                .unwrap_or(4);
            writeln!(out, "{:id_width$}  {:name_width$}  ITEMS", "ID", "NAME")?;
            for scene in scenes {
                writeln!(
                    out,
                    "{:id_width$}  {:name_width$}  {}",
                    scene.scene_id.as_str(),
                    scene.name,
                    scene.item_count
                )?;
            }
        }
        QueryResult::OutputStatus { output_id, state } => {
            writeln!(out, "{output_id}: {}", wire_name(state))?;
        }
        other => line(out, other)?,
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
        } => {
            write!(out, "state.changed rev {rev} by {client_id}")?;
            if let Some(request_id) = request_id {
                write!(out, " ({request_id})")?;
            }
            writeln!(out)?;
        }
        Event::OutputStateChanged { output_id, state } => {
            writeln!(out, "output.state_changed {output_id} {}", wire_name(state))?;
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
        };
        writeln!(err, "{body}")
    } else {
        writeln!(err, "error: {error}")
    };
}

/// Golden-output tests against the mock engine, in their own file for size.
#[cfg(test)]
mod tests;
