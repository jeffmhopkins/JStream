//! The tool table, generated from the protocol's JSON Schema, and the decoding of tool
//! arguments into protocol calls.
//!
//! Tools are grouped by noun (docs/ARCHITECTURE.md#mcp): every edit, action, query and
//! history call (`history.undo`, `history.redo`) in `jstream_protocol::json_schema()` is
//! named `noun.verb`, and the `noun` tool takes it as its `op` argument. So a command added to the protocol shows up here with no code
//! change. Three fixed tools sit beside the groups: `batch` (several edits, atomically),
//! `describe_api` (the whole protocol schema) and `events` (the engine's events, read
//! with a cursor).

use crate::events::{BUFFER_CAPACITY, PAGE_SIZE};
use jstream_protocol::{
    Action, Batch, Call, Edit, HistoryCall, HistoryOp, IdempotencyKey, ProtocolError, Query, Rev,
    json_schema,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The tool that applies several edits atomically.
pub(crate) const BATCH_TOOL: &str = "batch";
/// The tool that returns the whole protocol schema.
pub(crate) const DESCRIBE_TOOL: &str = "describe_api";
/// The tool that returns the engine's events after a cursor.
pub(crate) const EVENTS_TOOL: &str = "events";

/// How the protocol classifies an op (ADR-0013), plus queries, which only read, and
/// history calls, which undo or redo the session's own steps (ADR-0032).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpClass {
    Edit,
    Action,
    Query,
    History,
}

impl OpClass {
    fn label(self) -> &'static str {
        match self {
            OpClass::Edit => "edit",
            OpClass::Action => "action",
            OpClass::Query => "query",
            OpClass::History => "history call",
        }
    }

    /// Whether the op takes `expected_rev` and `idempotency_key`.
    fn takes_rev(self) -> bool {
        matches!(self, OpClass::Edit | OpClass::History)
    }
}

/// One protocol command or query that a group tool accepts as its `op`.
#[derive(Clone, Debug)]
struct Op {
    /// The protocol name (`scene.create`), as in the `type` tag.
    name: String,
    class: OpClass,
}

#[derive(Debug)]
enum ToolKind {
    Batch,
    Group(Vec<Op>),
    Describe,
    Events,
}

#[derive(Debug)]
struct Tool {
    name: String,
    kind: ToolKind,
    /// The MCP `Tool` object returned by `tools/list`.
    definition: Value,
}

/// What a tool call asks for, once its arguments are decoded.
#[derive(Debug, PartialEq)]
pub(crate) enum Invocation {
    /// A protocol call, for [`Control::call`](jstream_control::Control::call).
    Call(Call),
    /// The protocol schema, which needs no engine.
    Describe,
    /// The buffered events after the cursor `after` (0 for all).
    Events { after: u64 },
}

/// Every tool, in the order `tools/list` returns them (sorted by name).
#[derive(Debug)]
pub(crate) struct Tools {
    tools: Vec<Tool>,
    /// Which group tool owns each op name, for pointing a misrouted op to the right tool.
    owners: BTreeMap<String, String>,
}

impl Tools {
    /// Generates the tools from the protocol schema.
    pub(crate) fn generate() -> Tools {
        let schema = json_schema();
        let empty = Map::new();
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .unwrap_or(&empty);

        let mut groups: BTreeMap<String, Vec<(Op, Value)>> = BTreeMap::new();
        for (def, class) in [
            ("Edit", OpClass::Edit),
            ("Action", OpClass::Action),
            ("Query", OpClass::Query),
            ("HistoryOp", OpClass::History),
        ] {
            for variant in variants(defs, def) {
                let Some(name) = variant
                    .pointer("/properties/type/const")
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let noun = name.split_once('.').map_or(name, |(noun, _)| noun);
                let op = Op {
                    name: name.to_owned(),
                    class,
                };
                groups
                    .entry(noun.to_owned())
                    .or_default()
                    .push((op, variant.clone()));
            }
        }

        let mut tools = vec![batch_tool(defs, &groups), describe_tool(), events_tool()];
        let mut owners = BTreeMap::new();
        for (noun, ops) in &groups {
            for (op, _) in ops {
                owners.insert(op.name.clone(), noun.clone());
            }
            tools.push(group_tool(defs, noun, ops));
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Tools { tools, owners }
    }

    /// The MCP `Tool` objects, for `tools/list`.
    pub(crate) fn definitions(&self) -> Vec<Value> {
        self.tools.iter().map(|t| t.definition.clone()).collect()
    }

    /// The group nouns, in order, for the server instructions.
    pub(crate) fn groups(&self) -> impl Iterator<Item = &str> {
        self.tools
            .iter()
            .filter(|t| matches!(t.kind, ToolKind::Group(_)))
            .map(|t| t.name.as_str())
    }

    /// Decodes a call of tool `name` with `arguments`. `None` if there is no such tool.
    /// Arguments that don't fit the tool fail with `malformed`, the protocol's code for a
    /// message it can't parse, so the agent sees the same code either way.
    pub(crate) fn decode(
        &self,
        name: &str,
        arguments: Value,
    ) -> Option<Result<Invocation, ProtocolError>> {
        let tool = self.tools.iter().find(|t| t.name == name)?;
        Some(match &tool.kind {
            ToolKind::Batch => decode_batch(arguments),
            ToolKind::Describe => match arguments.as_object() {
                Some(args) if args.is_empty() => Ok(Invocation::Describe),
                _ => Err(malformed("`describe_api` takes no arguments")),
            },
            ToolKind::Events => from_value::<EventsArgs>(arguments, "arguments")
                .map(|args| Invocation::Events { after: args.after }),
            ToolKind::Group(ops) => self.decode_op(&tool.name, ops, arguments),
        })
    }

    fn decode_op(
        &self,
        tool: &str,
        ops: &[Op],
        arguments: Value,
    ) -> Result<Invocation, ProtocolError> {
        let args: OpArgs = from_value(arguments, "arguments")?;
        let expected = || {
            ops.iter()
                .map(|op| format!("`{}`", op.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let Some(tag) = args.op.get("type").and_then(Value::as_str) else {
            return Err(malformed(format!(
                "`op.type` is missing; expected one of {}",
                expected()
            )));
        };
        let Some(op) = ops.iter().find(|op| op.name == tag) else {
            return Err(malformed(match self.owners.get(tag) {
                Some(owner) => format!("`{tag}` belongs to the `{owner}` tool, not `{tool}`"),
                None => format!("unknown op `{tag}`; expected one of {}", expected()),
            }));
        };
        if !op.class.takes_rev() && (args.expected_rev.is_some() || args.idempotency_key.is_some())
        {
            return Err(malformed(format!(
                "`expected_rev` and `idempotency_key` apply to edits and history calls only, \
                 and `{tag}` is {} {}",
                if op.class == OpClass::Action {
                    "an"
                } else {
                    "a"
                },
                op.class.label()
            )));
        }
        let call = match op.class {
            OpClass::Edit => Call::Batch(Batch {
                edits: vec![from_value::<Edit>(args.op, "op")?],
                expected_rev: args.expected_rev,
                idempotency_key: args.idempotency_key,
            }),
            OpClass::Action => Call::Action {
                action: from_value::<Action>(args.op, "op")?,
            },
            OpClass::Query => Call::Query {
                query: from_value::<Query>(args.op, "op")?,
            },
            OpClass::History => Call::History(HistoryCall {
                op: from_value::<HistoryOp>(args.op, "op")?,
                expected_rev: args.expected_rev,
                idempotency_key: args.idempotency_key,
            }),
        };
        Ok(Invocation::Call(call))
    }
}

/// A group tool's arguments. Unknown keys are rejected, so a misspelt `expected_rev`
/// can't silently drop the concurrency check.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpArgs {
    op: Value,
    #[serde(default)]
    expected_rev: Option<Rev>,
    #[serde(default)]
    idempotency_key: Option<IdempotencyKey>,
}

/// The `batch` tool's arguments: a [`Batch`], with unknown keys rejected.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchArgs {
    edits: Vec<Edit>,
    #[serde(default)]
    expected_rev: Option<Rev>,
    #[serde(default)]
    idempotency_key: Option<IdempotencyKey>,
}

/// The `events` tool's arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsArgs {
    #[serde(default)]
    after: u64,
}

fn decode_batch(arguments: Value) -> Result<Invocation, ProtocolError> {
    let args: BatchArgs = from_value(arguments, "arguments")?;
    Ok(Invocation::Call(Call::Batch(Batch {
        edits: args.edits,
        expected_rev: args.expected_rev,
        idempotency_key: args.idempotency_key,
    })))
}

fn from_value<T: DeserializeOwned>(value: Value, what: &str) -> Result<T, ProtocolError> {
    serde_json::from_value(value).map_err(|e| malformed(format!("{what}: {e}")))
}

fn malformed(reason: impl Into<String>) -> ProtocolError {
    ProtocolError::Malformed {
        reason: reason.into(),
    }
}

/// The `oneOf` variants of definition `def`.
fn variants<'a>(defs: &'a Map<String, Value>, def: &str) -> impl Iterator<Item = &'a Value> {
    defs.get(def)
        .and_then(|d| d.get("oneOf"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// A property schema of the protocol's `Batch` (`expected_rev`, `idempotency_key`).
fn batch_property(defs: &Map<String, Value>, name: &str) -> Value {
    defs.get("Batch")
        .and_then(|b| b.pointer(&format!("/properties/{name}")))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn batch_tool(defs: &Map<String, Value>, groups: &BTreeMap<String, Vec<(Op, Value)>>) -> Tool {
    let mut schema = defs.get("Batch").cloned().unwrap_or_else(|| json!({}));
    if let Some(object) = schema.as_object_mut() {
        object.remove("description");
        object.insert("additionalProperties".into(), Value::Bool(false));
    }
    let editing: Vec<String> = groups
        .iter()
        .filter(|(_, ops)| ops.iter().any(|(op, _)| op.class == OpClass::Edit))
        .map(|(noun, _)| format!("`{noun}`"))
        .collect();
    let description = format!(
        "Applies one or more edits atomically, as one undo step: either all of them apply \
         or none does, and the state `rev` goes up by exactly one. The edits are those the \
         {} tools take as `op`. Actions and queries can't be batched. Pass `expected_rev` \
         to apply only if nobody changed the state since you read it, and \
         `idempotency_key` to make a retry safe. Failures come back as a tool error whose \
         `error.code` is stable.",
        editing.join(", ")
    );
    Tool {
        name: BATCH_TOOL.into(),
        kind: ToolKind::Batch,
        definition: definition(BATCH_TOOL, &description, with_defs(schema, defs)),
    }
}

fn describe_tool() -> Tool {
    let description = "Returns the full JSON Schema of the JStream control protocol: every \
                       command, query, result, event and error code. Use it for details \
                       the other tools' schemas leave out.";
    Tool {
        name: DESCRIBE_TOOL.into(),
        kind: ToolKind::Describe,
        definition: definition(
            DESCRIBE_TOOL,
            description,
            json!({"type": "object", "additionalProperties": false}),
        ),
    }
}

fn events_tool() -> Tool {
    let description = format!(
        "Returns the engine's events after the cursor `after`, oldest first, without \
         waiting: `state.changed` (the state `rev` moved; `client_id` and `request_id` \
         say whose batch did it) and `output.state_changed`, from every client, yours \
         included, and `events.dropped` (the server dropped `count` events because this \
         session fell behind; re-read the state). The once-a-second `stats.tick` \
         is not in `events`: each read returns the newest one as `stats` (running drop \
         totals). Each comes with a `seq`, a sequence number of this MCP server, starting \
         at 1 when your first call opened the session. Pass the returned `next` as \
         `after` to read on; omit `after` (or pass 0) to read from the oldest kept. At \
         most {PAGE_SIZE} events come back at once; `more` says others are waiting. The \
         server keeps the last {BUFFER_CAPACITY}: `dropped` counts events after your \
         cursor that were discarded before you read them, so re-read the state if it is \
         not 0. `ended` means the session is gone and no more events will come."
    );
    Tool {
        name: EVENTS_TOOL.into(),
        kind: ToolKind::Events,
        definition: definition(
            EVENTS_TOOL,
            &description,
            json!({
                "type": "object",
                "properties": {
                    "after": {
                        "description": "The `next` of your previous read; 0 or omitted \
                                        for the oldest kept event.",
                        "type": "integer",
                        "minimum": 0,
                    }
                },
                "additionalProperties": false,
            }),
        ),
    }
}

fn group_tool(defs: &Map<String, Value>, noun: &str, ops: &[(Op, Value)]) -> Tool {
    let has = |class| ops.iter().any(|(op, _)| op.class == class);
    let list: Vec<String> = ops
        .iter()
        .map(|(op, _)| format!("`{}` ({})", op.name, op.class.label()))
        .collect();
    let mut description = format!(
        "Runs one `{noun}` operation: set `op.type` to one of {}.",
        list.join(", ")
    );
    let mut properties = Map::new();
    properties.insert(
        "op".into(),
        json!({
            "description": format!("The `{noun}` operation, tagged by `type`."),
            "oneOf": ops.iter().map(|(_, schema)| schema.clone()).collect::<Vec<_>>(),
        }),
    );
    if has(OpClass::History) {
        description.push_str(
            " Undo and redo apply to this session's own edits only, never another client's: \
             each raises the state `rev` by one, and is refused with `undo_conflict` when \
             another client has since changed what the step touched (the step is then \
             dropped), or `history_empty` when there is nothing to undo or redo. They \
             accept `expected_rev` and `idempotency_key`.",
        );
    }
    if has(OpClass::Edit) || has(OpClass::History) {
        for name in ["expected_rev", "idempotency_key"] {
            properties.insert(name.into(), batch_property(defs, name));
        }
    }
    if has(OpClass::Edit) {
        description.push_str(
            " An edit is applied as a batch of one: it is undoable, it raises the state `rev` \
             by one, and it accepts `expected_rev` and `idempotency_key`. Use `batch` to \
             apply several edits atomically.",
        );
    }
    if has(OpClass::Action) {
        description.push_str(
            " An action has side effects and is never undone; the answer only says it was \
             accepted.",
        );
    }
    if has(OpClass::Query) {
        description.push_str(" A query only reads.");
    }
    if ops.iter().any(|(op, _)| op.name == "video.snapshot") {
        // The schema says `png` is base64; over MCP it is moved out (#124 review N1).
        description.push_str(
            " A `video.snapshot` result leads with the PNG as an image content block; its \
             JSON `png` field only names that block.",
        );
    }
    description.push_str(" Failures come back as a tool error whose `error.code` is stable.");
    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": ["op"],
        "additionalProperties": false,
    });
    Tool {
        name: noun.to_owned(),
        kind: ToolKind::Group(ops.iter().map(|(op, _)| op.clone()).collect()),
        definition: definition(noun, &description, with_defs(schema, defs)),
    }
}

fn definition(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema})
}

/// Adds to `schema` a `$defs` holding every protocol definition it refers to, directly or
/// through other definitions, so each tool's schema stands alone.
fn with_defs(mut schema: Value, defs: &Map<String, Value>) -> Value {
    let mut needed = BTreeSet::new();
    let mut pending = Vec::new();
    refs(&schema, &mut pending);
    while let Some(name) = pending.pop() {
        if let Some(def) = defs.get(&name)
            && needed.insert(name)
        {
            refs(def, &mut pending);
        }
    }
    if !needed.is_empty()
        && let Some(object) = schema.as_object_mut()
    {
        let picked: Map<String, Value> = needed
            .into_iter()
            .filter_map(|name| defs.get(&name).map(|def| (name, def.clone())))
            .collect();
        object.insert("$defs".into(), Value::Object(picked));
    }
    schema
}

/// The definition names of every `#/$defs/<name>` reference in `value`.
fn refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                match (key.as_str(), value) {
                    ("$ref", Value::String(target)) => {
                        if let Some(name) = target.strip_prefix("#/$defs/") {
                            out.push(name.to_owned());
                        }
                    }
                    _ => refs(value, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| refs(v, out)),
        _ => {}
    }
}

/// R-API-002 for the generated tools; run by the crate-root `tool_names_are_valid`.
#[cfg(test)]
pub(crate) fn check_tool_names_are_valid() {
    fn is_valid_tool_name(name: &str) -> bool {
        (1..=128).contains(&name.len())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }
    fn is_snake(word: &str) -> bool {
        !word.is_empty()
            && word.starts_with(|c: char| c.is_ascii_lowercase())
            && word
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    let tools = Tools::generate();
    let mut names = BTreeSet::new();
    for tool in &tools.tools {
        assert!(is_valid_tool_name(&tool.name), "`{}` is invalid", tool.name);
        assert!(names.insert(tool.name.clone()), "`{}` twice", tool.name);
        assert_eq!(tool.definition["name"].as_str(), Some(tool.name.as_str()));
        if let ToolKind::Group(ops) = &tool.kind {
            for op in ops {
                let (noun, verb) = op.name.split_once('.').unwrap_or_default();
                assert!(is_snake(noun) && is_snake(verb), "`{}`", op.name);
                assert_eq!(noun, tool.name, "`{}` is in the wrong group", op.name);
            }
        }
    }
    // Every edit, action, query and history op in the schema reaches exactly one group
    // tool.
    let schema = json_schema();
    let defs = schema["$defs"].as_object().cloned().unwrap_or_default();
    let in_schema: usize = ["Edit", "Action", "Query", "HistoryOp"]
        .iter()
        .map(|def| variants(&defs, def).count())
        .sum();
    assert!(in_schema > 0);
    assert_eq!(tools.owners.len(), in_schema, "an op has no tool, or two");
}

#[cfg(test)]
mod tests {
    use super::{Invocation, Tools};
    use jstream_protocol::{
        Action, Batch, Call, Edit, HistoryCall, HistoryOp, IdempotencyKey, OutputId, ProtocolError,
        Query, Rev, SceneId,
    };
    use serde_json::{Value, json};

    fn decode(tool: &str, args: Value) -> Result<Invocation, ProtocolError> {
        Tools::generate()
            .decode(tool, args)
            .expect("the tool exists")
    }

    fn reason(result: Result<Invocation, ProtocolError>) -> String {
        match result {
            Err(ProtocolError::Malformed { reason }) => reason,
            other => panic!("expected malformed, got {other:?}"),
        }
    }

    #[test]
    fn group_ops_map_to_calls_by_class() {
        let edit = decode(
            "scene",
            json!({"op": {"type": "scene.create", "scene_id": "s", "name": "Main"},
                   "expected_rev": 3, "idempotency_key": "k"}),
        );
        let batch = Batch::single(Edit::SceneCreate {
            scene_id: SceneId::new("s"),
            name: "Main".into(),
        })
        .expecting(Rev(3))
        .with_idempotency_key(IdempotencyKey::new("k"));
        assert_eq!(edit, Ok(Invocation::Call(Call::Batch(batch))));

        let action = decode(
            "output",
            json!({"op": {"type": "output.start", "output_id": "o"}}),
        );
        let expected = Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::new("o"),
            },
        };
        assert_eq!(action, Ok(Invocation::Call(expected)));

        let query = decode("scene", json!({"op": {"type": "scene.list"}}));
        let expected = Call::Query {
            query: Query::SceneList,
        };
        assert_eq!(query, Ok(Invocation::Call(expected)));

        let undo = decode(
            "history",
            json!({"op": {"type": "history.undo"}, "expected_rev": 4, "idempotency_key": "u"}),
        );
        let expected = Call::History(
            HistoryCall::new(HistoryOp::Undo)
                .expecting(Rev(4))
                .with_idempotency_key(IdempotencyKey::new("u")),
        );
        assert_eq!(undo, Ok(Invocation::Call(expected)));
        let redo = decode("history", json!({"op": {"type": "history.redo"}}));
        assert_eq!(
            redo,
            Ok(Invocation::Call(Call::History(HistoryCall::new(
                HistoryOp::Redo
            ))))
        );
        assert_eq!(decode("describe_api", json!({})), Ok(Invocation::Describe));
        assert_eq!(
            decode("events", json!({})),
            Ok(Invocation::Events { after: 0 })
        );
        assert_eq!(
            decode("events", json!({"after": 12})),
            Ok(Invocation::Events { after: 12 })
        );
    }

    #[test]
    fn bad_arguments_are_malformed_with_a_useful_reason() {
        let r = reason(decode("scene", json!({})));
        assert!(r.contains("missing field `op`"), "{r}");
        let r = reason(decode(
            "scene",
            json!({"op": {"type": "item.remove", "item_id": "i"}}),
        ));
        assert!(r.contains("belongs to the `item` tool"), "{r}");
        let r = reason(decode("scene", json!({"op": {"type": "scene.explode"}})));
        assert!(
            r.contains("unknown op") && r.contains("`scene.create`"),
            "{r}"
        );
        let r = reason(decode("scene", json!({"op": {"name": "x"}})));
        assert!(r.contains("`op.type` is missing"), "{r}");
        let r = reason(decode(
            "scene",
            json!({"op": {"type": "scene.rename", "scene_id": "s"}}),
        ));
        assert!(r.starts_with("op: missing field `name`"), "{r}");
        let r = reason(decode(
            "scene",
            json!({"op": {"type": "scene.list"}, "expected_rev": 1}),
        ));
        assert!(
            r.contains("edits and history calls only") && r.contains("a query"),
            "{r}"
        );
        let r = reason(decode(
            "output",
            json!({"op": {"type": "output.stop", "output_id": "o"}, "idempotency_key": "k"}),
        ));
        assert!(r.contains("an action"), "{r}");
        let r = reason(decode(
            "scene",
            json!({"op": {"type": "scene.list"}, "expectedRev": 1}),
        ));
        assert!(r.contains("unknown field `expectedRev`"), "{r}");
        let r = reason(decode(
            "batch",
            json!({"edits": [{"type": "output.start", "output_id": "o"}]}),
        ));
        assert!(r.starts_with("arguments:"), "{r}");
        let r = reason(decode("describe_api", json!({"verbose": true})));
        assert!(r.contains("no arguments"), "{r}");
        let r = reason(decode("events", json!({"after": -1})));
        assert!(r.starts_with("arguments:"), "{r}");
        let r = reason(decode("events", json!({"since": 3})));
        assert!(r.contains("unknown field `since`"), "{r}");
        let r = reason(decode("scene", json!("scene.list")));
        assert!(r.starts_with("arguments:"), "{r}");
    }

    #[test]
    fn unknown_tools_are_none() {
        assert!(
            Tools::generate()
                .decode("scene_create", json!({}))
                .is_none()
        );
    }

    #[test]
    fn every_tool_schema_is_a_standalone_object_schema() {
        for tool in Tools::generate().definitions() {
            let schema = &tool["inputSchema"];
            assert_eq!(schema["type"], "object", "{}", tool["name"]);
            let mut refs = Vec::new();
            super::refs(schema, &mut refs);
            for name in refs {
                assert!(
                    schema["$defs"].get(&name).is_some(),
                    "{}: `{name}` is not in its $defs",
                    tool["name"]
                );
            }
        }
    }
}
