//! The protocol's JSON Schema, generated from the Rust types.

use crate::envelope::{ClientMessage, ServerMessage};
use crate::version::PROTOCOL_VERSION;
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

/// The JSON Schema (draft 2020-12) of every message on a connection.
///
/// The root accepts any [`ClientMessage`] or [`ServerMessage`]. Each type is a named
/// definition under `$defs`, so tools (the MCP server's `describe_api`, code generators)
/// can pick out the parts they need. The schema is snapshot-tested: any change to the API
/// shows up as a diff of `crates/jstream-protocol/schema/protocol.snap` (R-TEST-002).
pub fn json_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let client = generator.subschema_for::<ClientMessage>();
    let server = generator.subschema_for::<ServerMessage>();
    let defs = generator.take_definitions(true);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": format!("JStream protocol {PROTOCOL_VERSION}"),
        "description": "Any message on a JStream control connection: client to server, or server to client.",
        "anyOf": [client, server],
        "$defs": defs,
    })
}
