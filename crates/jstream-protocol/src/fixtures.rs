//! Test fixtures: one value of every message, command, query, event and error variant,
//! and the checks the crate-root tests run over them.
//!
//! The fixtures are checked against the generated schema, so a variant added without a
//! fixture fails `fixtures_cover_every_variant_in_the_schema`, and through it the
//! naming and classification checks see every variant.

use crate::{
    Action, Batch, Call, CallResult, Canvas, ClientId, ClientMessage, Command, CommandClass, Crop,
    DropCounts, Edit, EntityKind, ErrorResponse, Event, EventMessage, Finite32, FitMode, Hello,
    IdempotencyKey, ItemGeometry, ItemId, ItemInfo, ItemTransform, OutputId, OutputInfo,
    OutputKind, OutputState, PROTOCOL_VERSION, PlacementForm, ProtocolError, Query, QueryResult,
    Request, RequestId, Response, Rev, RtmpTarget, SceneId, SceneInfo, SceneSummary, SecretString,
    ServerMessage, Settings, Size, SourceId, SourceInfo, SourceKind, Vec2, Welcome, json_schema,
};
use crate::{
    EnumOption, KindCategory, KindDescriptor, KindSummary, Media, ParamDescriptor, ParamType,
    VisibleIf,
};
use serde_json::{Value, json};
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
            geometry: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("scn_1"),
            item_id: ItemId::new("itm_2"),
            source_id: SourceId::new("src_cam"),
            transform: Some(ItemTransform::IDENTITY),
            geometry: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("scn_1"),
            item_id: ItemId::new("itm_3"),
            source_id: SourceId::new("src_cam"),
            transform: None,
            geometry: Some(corner_box()),
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
        Edit::ItemSetGeometry {
            item_id: ItemId::new("itm_2"),
            geometry: ItemGeometry {
                fit_mode: FitMode::Fill,
                rotation_deg: Finite32::new(12.5).expect("finite"),
                ..ItemGeometry::covering_canvas(1920, 1080).expect("positive")
            },
        },
        Edit::CanvasSetSize {
            width: 3840,
            height: 2160,
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
            settings: Settings::new(),
        },
        Edit::SourceCreate {
            source_id: SourceId::new("src_bg"),
            name: "Background".into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: color_settings(),
        },
        Edit::SourceCreate {
            source_id: SourceId::new("src_intro"),
            name: "Intro scene".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("scn_1"),
            },
            settings: Settings::new(),
        },
        Edit::SourceRename {
            source_id: SourceId::new("src_bg"),
            name: "Backdrop".into(),
        },
        Edit::SourceSetSettings {
            source_id: SourceId::new("src_logo"),
            patch: [
                (
                    "file",
                    json!({"type": "asset", "root": "collection", "path": "img/logo.png"}),
                ),
                ("mask", json!({"type": "scene", "scene_id": "scn_mask"})),
                ("tint", Value::Null),
            ]
            .into_iter()
            .collect(),
        },
        Edit::SourceDuplicate {
            source_id: SourceId::new("src_cam"),
            new_source_id: SourceId::new("src_cam_2"),
            name: "Camera 2".into(),
        },
        Edit::SourceDelete {
            source_id: SourceId::new("src_cam_2"),
        },
        Edit::OutputCreate {
            output_id: OutputId::new("out_yt"),
            kind: rtmp("rtmp://a.rtmp.youtube.com/live2"),
        },
        Edit::OutputSetTarget {
            output_id: OutputId::new("out_yt"),
            target: RtmpTarget::Generic {
                url: "rtmps://a.rtmps.youtube.com:443/live2".into(),
            },
        },
        Edit::OutputDelete {
            output_id: OutputId::new("out_yt"),
        },
    ]
}

/// An RTMP output kind publishing to `url`.
fn rtmp(url: &str) -> OutputKind {
    OutputKind::Rtmp {
        target: RtmpTarget::Generic { url: url.into() },
    }
}

fn output_infos() -> Vec<OutputInfo> {
    vec![OutputInfo {
        output_id: OutputId::new("out_yt"),
        kind: rtmp("rtmp://a.rtmp.youtube.com/live2"),
    }]
}

pub(crate) fn actions() -> Vec<Action> {
    vec![
        Action::OutputStart {
            output_id: OutputId::new("out_yt"),
        },
        Action::OutputStop {
            output_id: OutputId::new("out_yt"),
        },
        Action::OutputSetKey {
            output_id: OutputId::new("out_yt"),
            key: SecretString::new("fixture-key-0000"),
        },
        Action::OutputClearKey {
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
        Query::KindList,
        Query::KindDescribe {
            kind_id: "color".into(),
        },
        Query::OutputList,
    ]
}

fn color_settings() -> Settings {
    [("color", json!("#1e1e2eff")), ("width", json!(1920))]
        .into_iter()
        .collect()
}

/// A descriptor using every parameter type and hint.
fn color_kind() -> KindDescriptor {
    let param = |id: &str, ty: ParamType, default: Value| ParamDescriptor {
        id: id.into(),
        ty,
        default,
        label: id.into(),
        description: None,
        group: None,
        advanced: false,
        visible_if: None,
        live: true,
    };
    KindDescriptor {
        kind_id: "color".into(),
        category: KindCategory::Source,
        name: "Colour".into(),
        version: 1,
        video: true,
        audio: false,
        params: vec![
            ParamDescriptor {
                description: Some("Fill colour.".into()),
                group: Some("Look".into()),
                ..param(
                    "color",
                    ParamType::Color { alpha: true },
                    json!("#000000ff"),
                )
            },
            ParamDescriptor {
                live: false,
                ..param(
                    "width",
                    ParamType::Int {
                        min: Some(1),
                        max: Some(16384),
                        step: Some(1),
                        unit: Some("px".into()),
                    },
                    json!(1920),
                )
            },
            param(
                "opacity",
                ParamType::Float {
                    min: Some(0.0),
                    max: Some(1.0),
                    step: Some(0.01),
                    unit: None,
                },
                json!(1.0),
            ),
            param("dither", ParamType::Bool, json!(false)),
            ParamDescriptor {
                advanced: true,
                ..param(
                    "pattern",
                    ParamType::Enum {
                        options: vec![
                            EnumOption {
                                value: "solid".into(),
                                label: "Solid".into(),
                            },
                            EnumOption {
                                value: "checker".into(),
                                label: "Checker".into(),
                            },
                        ],
                    },
                    json!("solid"),
                )
            },
            ParamDescriptor {
                visible_if: Some(VisibleIf {
                    param: "pattern".into(),
                    equals: json!("checker"),
                }),
                ..param(
                    "caption",
                    ParamType::String {
                        multiline: false,
                        max_len: 64,
                    },
                    json!(""),
                )
            },
            param(
                "texture",
                ParamType::Asset {
                    accept: vec!["png".into()],
                },
                Value::Null,
            ),
            param("mask", ParamType::Scene, Value::Null),
            param(
                "follow",
                ParamType::Source {
                    media: Some(Media::Audio),
                },
                Value::Null,
            ),
            param(
                "key",
                ParamType::Source {
                    media: Some(Media::Video),
                },
                Value::Null,
            ),
            param("any", ParamType::Source { media: None }, Value::Null),
        ],
    }
}

fn source_infos() -> Vec<SourceInfo> {
    vec![
        SourceInfo {
            source_id: SourceId::new("src_bg"),
            name: "Background".into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: color_settings(),
        },
        SourceInfo {
            source_id: SourceId::new("src_cam"),
            name: "Camera".into(),
            kind: SourceKind::Builtin {
                kind: "webcam".into(),
            },
            settings: Settings::new(),
        },
        SourceInfo {
            source_id: SourceId::new("src_intro"),
            name: "Intro scene".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("scn_intro"),
            },
            settings: Settings::new(),
        },
    ]
}

/// A 480×270 box in the bottom-right quarter of a 1920×1080 canvas.
fn corner_box() -> ItemGeometry {
    ItemGeometry::try_new(
        Vec2::new(1600.0, 900.0).expect("finite"),
        Size::try_new(480.0, 270.0).expect("positive"),
    )
    .expect("positive")
}

fn item_infos() -> Vec<ItemInfo> {
    vec![
        ItemInfo {
            item_id: ItemId::new("itm_1"),
            source_id: SourceId::new("src_intro"),
            placement: PlacementForm::Geometry,
            transform: corner_box().transform_view().expect("finite"),
            geometry: Some(ItemGeometry {
                fit_mode: FitMode::Stretch,
                ..corner_box()
            }),
            crop: Crop::NONE,
            visible: true,
        },
        ItemInfo {
            item_id: ItemId::new("itm_2"),
            source_id: SourceId::new("src_cam"),
            placement: PlacementForm::Legacy,
            geometry: None,
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
            key_set: true,
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
            canvas: Canvas::DEFAULT,
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
            outputs: output_infos(),
        },
        // A 1×1 opaque black PNG, so the wire example stays small. The engine's
        // encoder may produce different bytes for the same pixels.
        QueryResult::VideoSnapshot {
            rev: Rev(12),
            width: 1,
            height: 1,
            png: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=".into(),
        },
        QueryResult::KindList {
            kinds: vec![
                color_kind().summary(),
                KindSummary {
                    kind_id: "fx/blur".into(),
                    category: KindCategory::VideoFilter,
                    name: "Blur".into(),
                    version: 2,
                },
                KindSummary {
                    kind_id: "gain".into(),
                    category: KindCategory::AudioFilter,
                    name: "Gain".into(),
                    version: 1,
                },
            ],
        },
        QueryResult::KindDescribe {
            settings_schema: color_kind().settings_schema(),
            kind: color_kind(),
        },
        QueryResult::OutputList {
            rev: Rev(12),
            outputs: output_infos(),
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
    events.push(Event::StatsTick {
        seq: 4,
        drops: DropCounts {
            capture_frames: 2,
            audio_frames: 5,
            render_ticks: 1,
            encode_frames: 4,
            encode_blocks: 6,
            output_packets: 7,
        },
    });
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
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::fmt::Debug;

    /// Whether `value` holds a `null` outside a settings value: under `settings`, `patch`,
    /// a parameter's `default` or `visible_if.equals`, or a generated `settings_schema`,
    /// `null` is data (no reference, reset to default).
    fn has_stray_null(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::Array(items) => items.iter().any(has_stray_null),
            Value::Object(map) => map.iter().any(|(key, v)| {
                !matches!(
                    key.as_str(),
                    "settings" | "patch" | "default" | "equals" | "settings_schema"
                ) && has_stray_null(v)
            }),
            _ => false,
        }
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
        let json = serde_json::to_string(value).expect("serializes");
        // No field is ever `null`: optional fields are omitted, and floats are finite.
        // Settings values are the exception: there `null` means no reference or the
        // default.
        let tree: Value = serde_json::from_str(&json).expect("parses as JSON");
        assert!(!has_stray_null(&tree), "serialized a null: {json}");
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
