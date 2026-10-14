//! Test fixtures: one value of every message, command, query, event and error variant,
//! and the checks the crate-root tests run over them.
//!
//! The fixtures are checked against the generated schema, so a variant added without a
//! fixture fails `fixtures_cover_every_variant_in_the_schema`, and through it the
//! naming and classification checks see every variant.

use crate::{
    Action, Batch, Call, CallResult, ClientId, ClientMessage, Command, CommandClass, Crop, Edit,
    EntityKind, ErrorResponse, Event, EventMessage, Finite32, Hello, IdempotencyKey, ItemId,
    ItemInfo, ItemTransform, OutputId, OutputState, PROTOCOL_VERSION, ProtocolError, Query,
    QueryResult, Request, RequestId, Response, Rev, SceneId, SceneInfo, SceneSummary,
    ServerMessage, SourceId, SourceInfo, SourceKind, Vec2, Welcome, json_schema,
};
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) fn edits() -> Vec<Edit> {
    vec![
        Edit::SceneCreate {
            scene_id: SceneId::new("scn_1"),
            name: "Main".into(),
        },
        Edit::SceneRename {
            scene_id: SceneId::new("scn_1"),
            name: "Intro".into(),
        },
        Edit::SceneDelete {
            scene_id: SceneId::new("scn_1"),
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("scn_1"),
            item_id: ItemId::new("itm_1"),
            source_id: SourceId::new("src_cam"),
            transform: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("scn_1"),
            item_id: ItemId::new("itm_2"),
            source_id: SourceId::new("src_cam"),
            transform: Some(ItemTransform::IDENTITY),
        },
        Edit::ItemRemove {
            item_id: ItemId::new("itm_1"),
        },
        Edit::ItemSetTransform {
            item_id: ItemId::new("itm_2"),
            transform: ItemTransform {
                position: Vec2::new(1280.0, 0.5).expect("finite"),
                scale: Vec2::new(0.25, f32::MAX).expect("finite"),
                rotation_deg: Finite32::new(-90.0).expect("finite"),
            },
        },
        Edit::ItemSetVisible {
            item_id: ItemId::new("itm_2"),
            visible: false,
        },
        Edit::ItemReorder {
            item_id: ItemId::new("itm_2"),
            index: 0,
        },
        Edit::SceneReorder {
            scene_id: SceneId::new("scn_1"),
            index: 2,
        },
        Edit::ItemSetCrop {
            item_id: ItemId::new("itm_2"),
            crop: Crop {
                left: 8,
                top: 0,
                right: 8,
                bottom: 120,
            },
        },
        Edit::SourceCreate {
            source_id: SourceId::new("src_cam"),
            name: "Camera".into(),
            kind: SourceKind::Builtin {
                kind: "webcam".into(),
            },
        },
        Edit::SourceCreate {
            source_id: SourceId::new("src_intro"),
            name: "Intro scene".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("scn_1"),
            },
        },
        Edit::SourceDuplicate {
            source_id: SourceId::new("src_cam"),
            new_source_id: SourceId::new("src_cam_2"),
            name: "Camera 2".into(),
        },
        Edit::SourceDelete {
            source_id: SourceId::new("src_cam_2"),
        },
    ]
}

pub(crate) fn actions() -> Vec<Action> {
    vec![
        Action::OutputStart {
            output_id: OutputId::new("out_yt"),
        },
        Action::OutputStop {
            output_id: OutputId::new("out_yt"),
        },
    ]
}

pub(crate) fn queries() -> Vec<Query> {
    vec![
        Query::SceneList,
        Query::OutputStatus {
            output_id: OutputId::new("out_yt"),
        },
        Query::SourceList,
        Query::ItemList {
            scene_id: SceneId::new("scn_1"),
        },
        Query::StateGet,
        Query::VideoSnapshot,
    ]
}

fn source_infos() -> Vec<SourceInfo> {
    vec![
        SourceInfo {
            source_id: SourceId::new("src_cam"),
            name: "Camera".into(),
            kind: SourceKind::Builtin {
                kind: "webcam".into(),
            },
        },
        SourceInfo {
            source_id: SourceId::new("src_intro"),
            name: "Intro scene".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("scn_intro"),
            },
        },
    ]
}

fn item_infos() -> Vec<ItemInfo> {
    vec![
        ItemInfo {
            item_id: ItemId::new("itm_1"),
            source_id: SourceId::new("src_intro"),
            transform: ItemTransform::IDENTITY,
            crop: Crop::NONE,
            visible: true,
        },
        ItemInfo {
            item_id: ItemId::new("itm_2"),
            source_id: SourceId::new("src_cam"),
            transform: ItemTransform {
                position: Vec2::new(1280.0, 720.0).expect("finite"),
                scale: Vec2::new(0.25, 0.25).expect("finite"),
                rotation_deg: Finite32::ZERO,
            },
            crop: Crop {
                left: 8,
                top: 0,
                right: 8,
                bottom: 0,
            },
            visible: false,
        },
    ]
}

pub(crate) fn query_results() -> Vec<QueryResult> {
    vec![
        QueryResult::SceneList {
            rev: Rev(12),
            scenes: vec![SceneSummary {
                scene_id: SceneId::new("scn_1"),
                name: "Main".into(),
                item_count: 2,
            }],
        },
        QueryResult::OutputStatus {
            output_id: OutputId::new("out_yt"),
            state: OutputState::Active,
        },
        QueryResult::SourceList {
            rev: Rev(12),
            sources: source_infos(),
        },
        QueryResult::ItemList {
            rev: Rev(12),
            scene_id: SceneId::new("scn_1"),
            items: item_infos(),
        },
        QueryResult::StateGet {
            rev: Rev(12),
            sources: source_infos(),
            scenes: vec![
                SceneInfo {
                    scene_id: SceneId::new("scn_1"),
                    name: "Main".into(),
                    items: item_infos(),
                },
                SceneInfo {
                    scene_id: SceneId::new("scn_intro"),
                    name: "Intro".into(),
                    items: vec![],
                },
            ],
        },
        // A 1×1 opaque black PNG, so the wire example stays small. The engine's
        // encoder may produce different bytes for the same pixels.
        QueryResult::VideoSnapshot {
            rev: Rev(12),
            width: 1,
            height: 1,
            png: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=".into(),
        },
    ]
}

pub(crate) fn events() -> Vec<Event> {
    let mut events = vec![
        Event::StateChanged {
            rev: Rev(13),
            client_id: ClientId::new("cli_7"),
            request_id: Some(RequestId::new("r1")),
        },
        Event::StateChanged {
            rev: Rev(14),
            client_id: ClientId::new("cli_8"),
            request_id: None,
        },
    ];
    for state in [
        OutputState::Stopped,
        OutputState::Starting,
        OutputState::Active,
        OutputState::Reconnecting,
        OutputState::Stopping,
        OutputState::Failed,
    ] {
        events.push(Event::OutputStateChanged {
            output_id: OutputId::new("out_yt"),
            state,
        });
    }
    events.push(Event::EventsDropped { count: 1500 });
    events
}

pub(crate) fn errors() -> Vec<ProtocolError> {
    let mut errors = vec![
        ProtocolError::UnsupportedVersion {
            client: crate::ProtocolVersion { major: 9, minor: 0 },
            server: PROTOCOL_VERSION,
        },
        ProtocolError::HandshakeRequired,
        ProtocolError::Malformed {
            reason: "missing field `id`".into(),
        },
        ProtocolError::EmptyBatch,
        ProtocolError::RevConflict {
            expected: Rev(4),
            current: Rev(5),
        },
        ProtocolError::InvalidArgument {
            field: "edits.0.name".into(),
            reason: "must not be empty".into(),
        },
        ProtocolError::ActionFailed {
            reason: "connection refused".into(),
        },
        ProtocolError::Internal {
            reason: "render thread stopped".into(),
        },
    ];
    for entity in [
        EntityKind::Scene,
        EntityKind::Item,
        EntityKind::Source,
        EntityKind::Output,
    ] {
        errors.push(ProtocolError::NotFound {
            entity,
            id: "x".into(),
        });
        errors.push(ProtocolError::AlreadyExists {
            entity,
            id: "x".into(),
        });
    }
    errors
}

pub(crate) fn client_messages() -> Vec<ClientMessage> {
    let mut msgs = vec![ClientMessage::Hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "jstream-cli".into(),
    })];
    let request = |n: usize, call| {
        ClientMessage::Request(Request {
            id: RequestId::new(format!("r{n}")),
            call,
        })
    };
    msgs.push(request(
        0,
        Call::Batch(Batch {
            edits: edits(),
            expected_rev: None,
            idempotency_key: None,
        }),
    ));
    msgs.push(request(
        1,
        Call::Batch(
            Batch::single(Edit::SceneDelete {
                scene_id: SceneId::new("scn_1"),
            })
            .expecting(Rev(12))
            .with_idempotency_key(IdempotencyKey::new("k-1")),
        ),
    ));
    for (n, action) in actions().into_iter().enumerate() {
        msgs.push(request(10 + n, Call::Action { action }));
    }
    for (n, query) in queries().into_iter().enumerate() {
        msgs.push(request(20 + n, Call::Query { query }));
    }
    msgs
}

pub(crate) fn server_messages() -> Vec<ServerMessage> {
    let mut msgs = vec![
        ServerMessage::Welcome(Welcome {
            protocol: PROTOCOL_VERSION,
            client_id: ClientId::new("cli_7"),
            rev: Rev(12),
        }),
        ServerMessage::Response(Response {
            id: RequestId::new("r0"),
            result: CallResult::Batch { rev: Rev(13) },
        }),
        ServerMessage::Response(Response {
            id: RequestId::new("r10"),
            result: CallResult::Action {},
        }),
        ServerMessage::Error(ErrorResponse {
            id: None,
            error: ProtocolError::HandshakeRequired.into(),
        }),
    ];
    for result in query_results() {
        msgs.push(ServerMessage::Response(Response {
            id: RequestId::new("r20"),
            result: CallResult::Query { result },
        }));
    }
    for error in errors() {
        msgs.push(ServerMessage::Error(ErrorResponse::for_request(
            RequestId::new("r1"),
            error,
        )));
    }
    for event in events() {
        msgs.push(ServerMessage::Event(EventMessage { event }));
    }
    msgs
}

/// Every command (edit or action), as the classification view.
fn commands() -> Vec<Command> {
    let mut commands: Vec<Command> = edits().into_iter().map(Command::from).collect();
    commands.extend(actions().into_iter().map(Command::from));
    commands
}

/// The `const` values of `tag` across the `oneOf` alternatives of definition `def`.
fn schema_tags(schema: &Value, def: &str, tag: &str) -> BTreeSet<String> {
    let alternatives = schema["$defs"][def]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("$defs.{def} has no oneOf"));
    alternatives
        .iter()
        .map(|alt| {
            alt["properties"][tag]["const"]
                .as_str()
                .unwrap_or_else(|| panic!("an alternative of {def} has no `{tag}` const"))
                .to_owned()
        })
        .collect()
}

/// `noun.verb`, both parts snake_case.
fn is_noun_verb(name: &str) -> bool {
    let snake = |part: &str| {
        part.starts_with(|c: char| c.is_ascii_lowercase())
            && !part.ends_with('_')
            && !part.contains("__")
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    matches!(name.split_once('.'), Some((noun, verb)) if snake(noun) && snake(verb))
}

/// The MCP tool-name pattern from R-API-002: `^[a-zA-Z0-9_-]{1,128}$`.
fn is_valid_mcp_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(crate) fn check_names_are_valid() {
    let schema = json_schema();
    let mut requests = BTreeSet::new();
    for name in schema_tags(&schema, "Edit", "type")
        .into_iter()
        .chain(schema_tags(&schema, "Action", "type"))
        .chain(schema_tags(&schema, "Query", "type"))
    {
        assert!(
            is_noun_verb(&name),
            "`{name}` is not noun.verb in snake_case"
        );
        let mcp = name.replace('.', "_");
        assert!(is_valid_mcp_name(&mcp), "MCP name `{mcp}` is invalid");
        assert!(requests.insert(name.clone()), "`{name}` is used twice");
    }
    for name in schema_tags(&schema, "Event", "type") {
        assert!(
            is_noun_verb(&name),
            "event `{name}` is not noun.verb in snake_case"
        );
    }
    // The Rust-side names agree with the wire tags.
    for (name, value) in edits()
        .iter()
        .map(|e| (e.name(), serde_json::to_value(e)))
        .chain(
            actions()
                .iter()
                .map(|a| (a.name(), serde_json::to_value(a))),
        )
        .chain(
            queries()
                .iter()
                .map(|q| (q.name(), serde_json::to_value(q))),
        )
        .chain(
            query_results()
                .iter()
                .map(|r| (r.name(), serde_json::to_value(r))),
        )
    {
        let value = value.expect("serializes");
        assert_eq!(value["type"].as_str(), Some(name));
    }
    for event in events() {
        let value = serde_json::to_value(&event).expect("serializes");
        assert_eq!(value["type"].as_str(), event.name());
    }
}

pub(crate) fn check_every_command_is_classified() {
    let schema = json_schema();
    let edit_names = schema_tags(&schema, "Edit", "type");
    let action_names = schema_tags(&schema, "Action", "type");
    assert!(
        edit_names.is_disjoint(&action_names),
        "a name is both an edit and an action"
    );
    // The only ways to send a command are a batch of edits or a single action.
    let call_kinds = schema_tags(&schema, "Call", "kind");
    assert_eq!(
        call_kinds,
        BTreeSet::from(["action".into(), "batch".into(), "query".into()])
    );

    let mut seen = BTreeSet::new();
    for command in commands() {
        let name = command.name();
        seen.insert(name.to_owned());
        match command.class() {
            CommandClass::Edit => {
                assert!(edit_names.contains(name), "{name} classified as an edit");
                assert!(command.is_undoable() && command.is_batchable(), "{name}");
            }
            CommandClass::Action => {
                assert!(
                    action_names.contains(name),
                    "{name} classified as an action"
                );
                assert!(!command.is_undoable() && !command.is_batchable(), "{name}");
            }
        }
    }
    let all: BTreeSet<String> = edit_names.union(&action_names).cloned().collect();
    assert_eq!(
        seen, all,
        "every command in the schema has a classified fixture"
    );
}

/// Every string in `value`, recursively.
fn strings_in(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(s) => {
            out.insert(s.clone());
        }
        Value::Array(items) => items.iter().for_each(|v| strings_in(v, out)),
        Value::Object(map) => map.values().for_each(|v| strings_in(v, out)),
        _ => {}
    }
}

/// Every tag and enum value the schema allows: each `const` and each `enum` entry.
fn schema_literals(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Array(items) => items.iter().for_each(|v| schema_literals(v, out)),
        Value::Object(map) => {
            if let Some(Value::String(c)) = map.get("const") {
                out.insert(c.clone());
            }
            if let Some(Value::Array(values)) = map.get("enum") {
                out.extend(values.iter().filter_map(|v| v.as_str().map(str::to_owned)));
            }
            map.values().for_each(|v| schema_literals(v, out));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{client_messages, schema_literals, server_messages, strings_in};
    use crate::{ClientMessage, ServerMessage, json_schema};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::collections::BTreeSet;
    use std::fmt::Debug;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
        let json = serde_json::to_string(value).expect("serializes");
        // No field is ever `null`: optional fields are omitted, and floats are finite.
        assert!(!json.contains("null"), "serialized a null: {json}");
        let back: T = serde_json::from_str(&json).expect("parses back");
        assert_eq!(&back, value, "round trip changed the value: {json}");
        let again = serde_json::to_string(&back).expect("serializes");
        assert_eq!(again, json, "serialization is not stable");
    }

    #[test]
    fn every_message_round_trips() {
        client_messages()
            .iter()
            .for_each(round_trip::<ClientMessage>);
        server_messages()
            .iter()
            .for_each(round_trip::<ServerMessage>);
    }

    #[test]
    fn fixtures_cover_every_variant_in_the_schema() {
        let mut used = BTreeSet::new();
        for msg in client_messages() {
            strings_in(&serde_json::to_value(&msg).expect("serializes"), &mut used);
        }
        for msg in server_messages() {
            strings_in(&serde_json::to_value(&msg).expect("serializes"), &mut used);
        }
        let mut allowed = BTreeSet::new();
        schema_literals(&json_schema(), &mut allowed);
        let missing: Vec<_> = allowed.difference(&used).collect();
        assert!(missing.is_empty(), "no fixture uses {missing:?}");
    }

    /// R-TEST-002: the schema snapshot is the API review. Regenerate it with
    /// `INSTA_UPDATE=always cargo test -p jstream-protocol` (or `cargo insta review`).
    #[test]
    fn schema_snapshot() {
        let schema = serde_json::to_string_pretty(&json_schema()).expect("serializes");
        insta::with_settings!({
            snapshot_path => "../schema",
            prepend_module_to_snapshot => false,
            omit_expression => true,
        }, {
            insta::assert_snapshot!("protocol", schema);
        });
    }

    /// Every fixture message, one JSON line each, so a wire-format change shows up as a
    /// readable diff next to the schema's.
    #[test]
    fn wire_examples_snapshot() {
        let mut lines = vec!["# client -> server".to_owned()];
        for msg in client_messages() {
            lines.push(serde_json::to_string(&msg).expect("serializes"));
        }
        lines.push("# server -> client".to_owned());
        for msg in server_messages() {
            lines.push(serde_json::to_string(&msg).expect("serializes"));
        }
        insta::with_settings!({
            snapshot_path => "../schema",
            prepend_module_to_snapshot => false,
            omit_expression => true,
        }, {
            insta::assert_snapshot!("examples", lines.join("\n"));
        });
    }
}
