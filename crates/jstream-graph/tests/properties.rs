//! Property tests of `Graph::apply`, with shrinking (proptest), through the public API
//! only.
//!
//! - **Inverse:** a batch that applies is undone exactly by its inverse, and redone
//!   exactly by the inverse's inverse; a batch that fails changes nothing.
//! - **Delete rejection:** after every batch, no reference dangles, and a plain delete of
//!   each scene, source and encoder profile is refused exactly when something still
//!   refers to it and changes nothing when refused; a scene's or source's refusal names
//!   every referrer as a blocker. Hotkey bindings are never blockers: a delete that
//!   applies takes the bindings naming its target with it (ADR-0040 §3), so none dangles.
//!
//! Edits are drawn from small ID pools, so they often hit existing objects, collide with
//! them or miss them, with names, numbers and settings values drawn freely. The seed is
//! fixed so CI is repeatable; set `PROPTEST_RNG_SEED` (and `PROPTEST_CASES`) to explore.
//! A failure is shrunk to a minimal batch sequence and saved under
//! `tests/properties.proptest-regressions`; commit that file with the fix.

use jstream_graph::Graph;
use jstream_protocol::Edit;
use jstream_protocol::{
    Action, Blocker, Chord, Crop, EncoderProfile, EncoderProfileId, EntityKind, Finite32, FitMode,
    ItemGeometry, ItemId, ItemTransform, KindCategory, KindDescriptor, Media, OutputId, OutputKind,
    ParamDescriptor, ParamType, ProtocolError, RateControl, RecordContainer, Reference, RtmpTarget,
    SceneId, SceneRole, SecretString, Settings, Size, SourceId, SourceKind, Vec2,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use serde_json::{Value, json};
use std::collections::HashSet;

/// References to one object, as `(entity, id, field)`.
type Referrers = HashSet<(EntityKind, String, String)>;

/// The seed CI runs with, unless `PROPTEST_RNG_SEED` is set.
const SEED: u64 = 0x4A53_7472_6561_6D51;

fn config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
        config.rng_seed = RngSeed::Fixed(SEED);
    }
    config
}

fn param(id: &str, ty: ParamType, default: Value) -> ParamDescriptor {
    ParamDescriptor {
        id: id.into(),
        ty,
        default,
        label: id.into(),
        description: None,
        group: None,
        advanced: false,
        visible_if: None,
        live: true,
    }
}

/// `color`, a video kind with a scalar, a reference of each sort (one restricted to
/// audio sources) and an asset; and `tone`, an audio kind with no parameters.
fn kinds() -> Vec<KindDescriptor> {
    let int = ParamType::Int {
        min: Some(1),
        max: Some(100),
        step: None,
        unit: None,
    };
    vec![
        KindDescriptor {
            kind_id: "color".into(),
            category: KindCategory::Source,
            name: "Colour".into(),
            version: 1,
            video: true,
            audio: false,
            params: vec![
                param(
                    "color",
                    ParamType::Color { alpha: true },
                    json!("#000000ff"),
                ),
                param("size", int, json!(10)),
                param("mask", ParamType::Scene, Value::Null),
                param("follow", ParamType::Source { media: None }, Value::Null),
                param(
                    "duck",
                    ParamType::Source {
                        media: Some(Media::Audio),
                    },
                    Value::Null,
                ),
                param("file", ParamType::Asset { accept: vec![] }, Value::Null),
            ],
        },
        KindDescriptor {
            kind_id: "tone".into(),
            category: KindCategory::Source,
            name: "Tone".into(),
            version: 1,
            video: false,
            audio: true,
            params: vec![],
        },
    ]
}

/// The graph every case starts from: each pool partly filled and every sort of
/// reference present, so random edits mostly meet objects that exist. Scenes `s0`–`s2`;
/// sources `c0` (`color`, its `mask` on `s1` and `follow` on `c1`), `c1` (`tone`) and
/// `c2` (showing `s2`); items `i0` and `i1` in `s0` placing `c0` and `c2`, and `i2` in
/// `s1` placing `c1`; profile `e0`, used by the RTMP output `o0`.
fn start() -> Graph {
    let scene = |id: &str| Edit::SceneCreate {
        scene_id: SceneId::new(id),
        name: format!("Scene {id}"),
        role: None,
    };
    let source = |id: &str, kind: SourceKind, settings: Settings| Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: format!("Source {id}"),
        kind,
        settings,
        duplicated_from: None,
    };
    let item = |scene: &str, id: &str, source: &str| Edit::ItemAdd {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(id),
        source_id: SourceId::new(source),
        transform: None,
        geometry: None,
    };
    let builtin = |kind: &str| SourceKind::Builtin { kind: kind.into() };
    let edits = vec![
        scene("s0"),
        scene("s1"),
        scene("s2"),
        source("c1", builtin("tone"), Settings::new()),
        source(
            "c0",
            builtin("color"),
            [
                ("mask", json!({"type": "scene", "scene_id": "s1"})),
                ("follow", json!({"type": "source", "source_id": "c1"})),
            ]
            .into_iter()
            .collect(),
        ),
        source(
            "c2",
            SourceKind::Scene {
                scene_id: SceneId::new("s2"),
            },
            Settings::new(),
        ),
        item("s0", "i0", "c0"),
        item("s0", "i1", "c2"),
        item("s1", "i2", "c1"),
        Edit::EncoderCreate {
            profile_id: EncoderProfileId::new("e0"),
            name: "Stream".into(),
            profile: EncoderProfile::DEFAULT,
        },
        Edit::OutputCreate {
            output_id: OutputId::new("o0"),
            kind: OutputKind::Rtmp {
                target: RtmpTarget::Generic {
                    url: "rtmp://a.example/live".into(),
                },
            },
        },
        Edit::OutputSetEncoder {
            output_id: OutputId::new("o0"),
            profile_id: Some(EncoderProfileId::new("e0")),
        },
    ];
    let mut graph = Graph::new().with_kinds(kinds());
    graph
        .apply(&edits, None)
        .expect("the starting batch applies");
    graph
}

/// An ID from a small pool, so edits often meet existing objects, and now and then any
/// string at all.
fn id(pool: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        30 => proptest::sample::select(pool).prop_map(str::to_owned),
        1 => any::<String>(),
    ]
    .boxed()
}

fn scene_id() -> BoxedStrategy<SceneId> {
    id(&["s0", "s1", "s2"]).prop_map(SceneId::new).boxed()
}

fn source_id() -> BoxedStrategy<SourceId> {
    id(&["c0", "c1", "c2", "c3"])
        .prop_map(SourceId::new)
        .boxed()
}

fn item_id() -> BoxedStrategy<ItemId> {
    id(&["i0", "i1", "i2", "i3", "i4"])
        .prop_map(ItemId::new)
        .boxed()
}

fn output_id() -> BoxedStrategy<OutputId> {
    id(&["o0", "o1"]).prop_map(OutputId::new).boxed()
}

fn profile_id() -> BoxedStrategy<EncoderProfileId> {
    id(&["e0", "e1"]).prop_map(EncoderProfileId::new).boxed()
}

/// A chord from a small pool, one of them misspelt.
fn chord() -> BoxedStrategy<Chord> {
    proptest::sample::select(
        &[
            "F1",
            "Ctrl+KeyA",
            "Shift+F2",
            "Ctrl+Digit1",
            "Shift+Ctrl+KeyA",
        ][..],
    )
    .prop_map(Chord::new)
    .boxed()
}

/// An action to bind, sometimes one that can't be bound.
fn action() -> BoxedStrategy<Action> {
    prop_oneof![
        3 => scene_id().prop_map(|scene_id| Action::SceneSwitch { scene_id }),
        2 => output_id().prop_map(|output_id| Action::OutputStart { output_id }),
        1 => output_id().prop_map(|output_id| Action::OutputStop { output_id }),
        1 => output_id().prop_map(|output_id| Action::OutputSetKey {
            output_id,
            key: SecretString::new("k"),
        }),
    ]
    .boxed()
}

/// Mostly short plain names; sometimes empty, long or anything.
fn name() -> BoxedStrategy<String> {
    prop_oneof![
        8 => proptest::sample::select(&["Main", "Camera", "Lower third", "é ✓"][..])
            .prop_map(str::to_owned),
        1 => any::<String>(),
    ]
    .boxed()
}

/// Any finite `f32`: non-finite draws become 0, which the protocol type can't hold.
fn finite() -> BoxedStrategy<Finite32> {
    prop_oneof![
        3 => (-4000i32..4000).prop_map(|n| n as f32 / 2.0),
        1 => any::<f32>(),
    ]
    .prop_map(|f| Finite32::new(f).unwrap_or(Finite32::ZERO))
    .boxed()
}

fn vec2() -> BoxedStrategy<Vec2> {
    (finite(), finite())
        .prop_map(|(x, y)| Vec2 { x, y })
        .boxed()
}

fn transform() -> BoxedStrategy<ItemTransform> {
    (vec2(), vec2(), finite())
        .prop_map(|(position, scale, rotation_deg)| ItemTransform {
            position,
            scale,
            rotation_deg,
        })
        .boxed()
}

/// Mostly a valid flat geometry; sometimes a zero or negative size, a tilt or a
/// perspective, which placement edits may refuse.
fn geometry() -> BoxedStrategy<ItemGeometry> {
    let fit = proptest::sample::select(&[FitMode::Fit, FitMode::Fill, FitMode::Stretch][..]);
    let side = prop_oneof![
        4 => (1u16..8000).prop_map(|n| Finite32::new(f32::from(n) / 2.0).unwrap_or(Finite32::ONE)),
        1 => finite(),
    ];
    let flat = prop_oneof![5 => Just(Finite32::ZERO), 1 => finite()];
    (
        vec2(),
        (side.clone(), side),
        fit,
        finite(),
        (flat.clone(), flat),
        proptest::option::weighted(0.15, finite()),
    )
        .prop_map(
            |(
                center,
                (width, height),
                fit_mode,
                rotation_deg,
                (tilt_deg, turn_deg),
                perspective_px,
            )| {
                ItemGeometry {
                    center,
                    size: Size { width, height },
                    fit_mode,
                    rotation_deg,
                    tilt_deg,
                    turn_deg,
                    perspective_px,
                    ..ItemGeometry::covering_canvas(1, 1).expect("a 1×1 canvas is valid")
                }
            },
        )
        .boxed()
}

fn crop() -> BoxedStrategy<Crop> {
    let side = prop_oneof![3 => 0u32..40, 1 => any::<u32>()];
    (side.clone(), side.clone(), side.clone(), side)
        .prop_map(|(left, top, right, bottom)| Crop {
            left,
            top,
            right,
            bottom,
        })
        .boxed()
}

fn index() -> BoxedStrategy<u32> {
    prop_oneof![4 => 0u32..6, 1 => any::<u32>()].boxed()
}

fn scene_ref() -> BoxedStrategy<Value> {
    scene_id()
        .prop_map(|id| json!({"type": "scene", "scene_id": id}))
        .boxed()
}

fn source_ref() -> BoxedStrategy<Value> {
    source_id()
        .prop_map(|id| json!({"type": "source", "source_id": id}))
        .boxed()
}

/// Anything a settings value could be: a reference of each sort, null, or a scalar.
fn any_value() -> BoxedStrategy<Value> {
    prop_oneof![
        scene_ref(),
        source_ref(),
        Just(Value::Null),
        Just(json!({"type": "asset", "root": "collection", "path": "img/a.png"})),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        any::<String>().prop_map(Value::from),
        any::<bool>().prop_map(Value::from),
    ]
    .boxed()
}

/// One `color` setting: mostly a value of the parameter's type (null resets it to the
/// default), now and then any parameter ID and any value.
fn setting() -> BoxedStrategy<(String, Value)> {
    let null_or = |s: BoxedStrategy<Value>| prop_oneof![3 => s, 1 => Just(Value::Null)];
    prop_oneof![
        2 => null_or(
            proptest::sample::select(&["#ff00ff", "#00000080", "#12345"][..])
                .prop_map(Value::from)
                .boxed()
        )
        .prop_map(|v| ("color".to_owned(), v)),
        2 => null_or(prop_oneof![4 => 1i64..=100, 1 => any::<i64>()].prop_map(Value::from).boxed())
            .prop_map(|v| ("size".to_owned(), v)),
        3 => null_or(scene_ref()).prop_map(|v| ("mask".to_owned(), v)),
        3 => null_or(source_ref()).prop_map(|v| ("follow".to_owned(), v)),
        2 => null_or(source_ref()).prop_map(|v| ("duck".to_owned(), v)),
        1 => Just(("file".to_owned(), json!({"type": "asset", "root": "collection", "path": "a.png"}))),
        1 => (
            proptest::sample::select(&["color", "size", "mask", "follow", "duck", "file", "other"][..]),
            any_value(),
        )
            .prop_map(|(k, v)| (k.to_owned(), v)),
    ]
    .boxed()
}

/// Mostly none (any kind takes that), else a few `color` settings.
fn settings() -> BoxedStrategy<Settings> {
    prop_oneof![
        2 => Just(Settings::new()),
        3 => proptest::collection::vec(setting(), 1..4).prop_map(|v| v.into_iter().collect()),
    ]
    .boxed()
}

fn source_kind() -> BoxedStrategy<SourceKind> {
    prop_oneof![
        6 => Just(SourceKind::Builtin { kind: "color".into() }),
        2 => Just(SourceKind::Builtin { kind: "tone".into() }),
        4 => scene_id().prop_map(|scene_id| SourceKind::Scene { scene_id }),
        1 => any::<String>().prop_map(|kind| SourceKind::Builtin { kind }),
    ]
    .boxed()
}

fn output_kind() -> BoxedStrategy<OutputKind> {
    prop_oneof![
        3 => rtmp_target().prop_map(|target| OutputKind::Rtmp { target }),
        1 => proptest::sample::select(&["/Videos/show.mp4", "show.mp4", ""][..]).prop_map(|p| {
            OutputKind::Record {
                path: p.into(),
                container: RecordContainer::Mp4,
            }
        }),
        1 => Just(OutputKind::VirtualCam),
    ]
    .boxed()
}

fn rtmp_target() -> BoxedStrategy<RtmpTarget> {
    prop_oneof![
        4 => proptest::sample::select(
            &[
                "rtmp://a.example/live",
                "rtmps://b.example:443/live",
                "rtmp://user:pass@c.example/live",
                "http://d.example/live",
            ][..]
        )
        .prop_map(str::to_owned),
        1 => any::<String>(),
    ]
    .prop_map(|url| RtmpTarget::Generic { url })
    .boxed()
}

/// A profile, its video rate sometimes outside what an engine can open.
fn profile() -> BoxedStrategy<EncoderProfile> {
    prop_oneof![3 => 100u32..=100_000, 1 => any::<u32>()]
        .prop_map(|kbps| {
            let mut profile = EncoderProfile::DEFAULT;
            profile.video.rate_control = RateControl::Cbr { kbps };
            profile
        })
        .boxed()
}

/// Addressing for a placement edit: usually none, sometimes a scene.
fn address() -> BoxedStrategy<Option<SceneId>> {
    prop_oneof![2 => Just(None), 1 => scene_id().prop_map(Some)].boxed()
}

/// Any edit. Creations are weighted up so states grow past a handful of objects.
fn edit() -> BoxedStrategy<Edit> {
    prop_oneof![
        4 => (scene_id(), name(), proptest::option::of(
            proptest::sample::select(&[SceneRole::Scene, SceneRole::Group][..])
        ))
            .prop_map(|(scene_id, name, role)| Edit::SceneCreate { scene_id, name, role }),
        1 => (scene_id(), name()).prop_map(|(scene_id, name)| Edit::SceneRename { scene_id, name }),
        1 => (scene_id(), proptest::sample::select(&[SceneRole::Scene, SceneRole::Group][..]))
            .prop_map(|(scene_id, role)| Edit::SceneSetRole { scene_id, role }),
        2 => scene_id().prop_map(|scene_id| Edit::SceneDelete { scene_id }),
        1 => (scene_id(), index()).prop_map(|(scene_id, index)| Edit::SceneReorder { scene_id, index }),
        6 => (
            scene_id(),
            item_id(),
            source_id(),
            proptest::option::of(transform()),
            proptest::option::weighted(0.3, geometry()),
        )
            .prop_map(|(scene_id, item_id, source_id, transform, geometry)| Edit::ItemAdd {
                scene_id,
                item_id,
                source_id,
                transform,
                geometry,
            }),
        2 => item_id().prop_map(|item_id| Edit::ItemRemove { item_id }),
        1 => (item_id(), scene_id(), index())
            .prop_map(|(item_id, scene_id, index)| Edit::ItemMove { item_id, scene_id, index }),
        1 => (item_id(), address(), transform()).prop_map(|(item_id, scene_id, transform)| {
            Edit::ItemSetTransform { item_id, scene_id, transform }
        }),
        1 => (item_id(), address(), geometry()).prop_map(|(item_id, scene_id, geometry)| {
            Edit::ItemSetGeometry { item_id, scene_id, geometry }
        }),
        1 => (item_id(), address(), any::<bool>()).prop_map(|(item_id, scene_id, visible)| {
            Edit::ItemSetVisible { item_id, scene_id, visible }
        }),
        1 => (item_id(), index()).prop_map(|(item_id, index)| Edit::ItemReorder { item_id, index }),
        1 => (item_id(), source_id())
            .prop_map(|(item_id, source_id)| Edit::ItemSetSource { item_id, source_id }),
        1 => (item_id(), address(), crop())
            .prop_map(|(item_id, scene_id, crop)| Edit::ItemSetCrop { item_id, scene_id, crop }),
        6 => (
            source_id(),
            name(),
            source_kind(),
            settings(),
            proptest::option::weighted(0.2, source_id()),
        )
            .prop_map(|(source_id, name, kind, settings, duplicated_from)| {
                Edit::SourceCreate { source_id, name, kind, settings, duplicated_from }
            }),
        1 => (source_id(), name())
            .prop_map(|(source_id, name)| Edit::SourceRename { source_id, name }),
        4 => (source_id(), settings())
            .prop_map(|(source_id, patch)| Edit::SourceSetSettings { source_id, patch }),
        1 => (source_id(), source_id(), name()).prop_map(|(source_id, new_source_id, name)| {
            Edit::SourceDuplicate { source_id, new_source_id, name }
        }),
        2 => source_id().prop_map(|source_id| Edit::SourceDelete { source_id }),
        2 => (output_id(), output_kind())
            .prop_map(|(output_id, kind)| Edit::OutputCreate { output_id, kind }),
        1 => (output_id(), rtmp_target())
            .prop_map(|(output_id, target)| Edit::OutputSetTarget { output_id, target }),
        1 => output_id().prop_map(|output_id| Edit::OutputDelete { output_id }),
        2 => (output_id(), proptest::option::of(profile_id()))
            .prop_map(|(output_id, profile_id)| Edit::OutputSetEncoder { output_id, profile_id }),
        2 => (profile_id(), name(), profile()).prop_map(|(profile_id, name, profile)| {
            Edit::EncoderCreate { profile_id, name, profile }
        }),
        1 => (profile_id(), name(), profile()).prop_map(|(profile_id, name, profile)| {
            Edit::EncoderUpdate { profile_id, name, profile }
        }),
        1 => profile_id().prop_map(|profile_id| Edit::EncoderDelete { profile_id }),
        1 => (prop_oneof![3 => 1u32..4000, 1 => any::<u32>()], prop_oneof![3 => 1u32..4000, 1 => any::<u32>()])
            .prop_map(|(width, height)| Edit::CanvasSetSize { width, height }),
        3 => (chord(), action()).prop_map(|(chord, action)| Edit::HotkeySet { chord, action }),
        1 => chord().prop_map(|chord| Edit::HotkeyClear { chord }),
    ]
    .boxed()
}

/// A run of batches of one to four edits. Most batches hold one edit, which applies more
/// often than a long one, so states grow.
fn batches() -> impl Strategy<Value = Vec<Vec<Edit>>> {
    let batch = prop_oneof![
        3 => proptest::collection::vec(edit(), 1..=1),
        1 => proptest::collection::vec(edit(), 2..=4),
    ];
    proptest::collection::vec(batch, 1..40)
}

/// Every reference in the state names something that exists. `duplicated_from` is a
/// plain link, not a reference, and may name a deleted source.
fn assert_no_dangling(graph: &Graph) {
    let state = graph.state();
    for scene in state.scenes() {
        for item in scene.items() {
            assert!(
                state.source(item.source_id()).is_some(),
                "item `{}` places missing source `{}`",
                item.id(),
                item.source_id()
            );
        }
    }
    for source in state.sources() {
        if let SourceKind::Scene { scene_id } = source.kind() {
            assert!(
                state.scene(scene_id).is_some(),
                "source `{}` shows missing scene `{scene_id}`",
                source.id()
            );
        }
        for (param, reference) in source.settings().references() {
            let exists = match &reference {
                Reference::Scene { scene_id } => state.scene(scene_id).is_some(),
                Reference::Source { source_id } => state.source(source_id).is_some(),
                _ => true,
            };
            assert!(
                exists,
                "source `{}` setting `{param}` dangles: {reference:?}",
                source.id()
            );
        }
    }
    for output in state.outputs() {
        if let Some(profile) = output.encoder_profile() {
            assert!(
                state.encoder(profile).is_some(),
                "output `{}` uses missing profile `{profile}`",
                output.id()
            );
        }
    }
    for (chord, action) in state.hotkeys() {
        for (entity, id) in action.named_objects() {
            let exists = match entity {
                EntityKind::Scene => state.scene(&SceneId::new(id)).is_some(),
                EntityKind::Output => state.output(&OutputId::new(id)).is_some(),
                EntityKind::Source => state.source(&SourceId::new(id)).is_some(),
                _ => false,
            };
            assert!(exists, "hotkey `{chord}` names missing {entity} `{id}`");
        }
    }
}

/// What still refers to a scene or source, worked out from the state independently of
/// the graph's own dependency code, as `(entity, id, field)`.
fn referrers(graph: &Graph, target: &Reference) -> Referrers {
    let state = graph.state();
    let mut out = Referrers::new();
    match target {
        Reference::Source { source_id } => {
            for scene in state.scenes() {
                for item in scene.items().iter().filter(|i| i.source_id() == source_id) {
                    out.insert((EntityKind::Item, item.id().to_string(), "source_id".into()));
                }
            }
        }
        Reference::Scene { scene_id } => {
            for source in state.sources() {
                if matches!(source.kind(), SourceKind::Scene { scene_id: s } if s == scene_id) {
                    out.insert((
                        EntityKind::Source,
                        source.id().to_string(),
                        "kind.scene_id".into(),
                    ));
                }
            }
        }
        _ => {}
    }
    for source in state.sources() {
        for (param, reference) in source.settings().references() {
            if &reference == target {
                out.insert((
                    EntityKind::Source,
                    source.id().to_string(),
                    format!("settings.{param}"),
                ));
            }
        }
    }
    out
}

fn blocker_set(blockers: &[Blocker]) -> Referrers {
    blockers
        .iter()
        .map(|b| (b.entity, b.id.clone(), b.field.clone()))
        .collect()
}

/// Tries one plain delete on a copy: refused exactly when `expected` is not empty, with
/// those blockers, and changing nothing.
fn check_delete(graph: &Graph, edit: Edit, expected: &Referrers) {
    let mut copy = graph.clone();
    match copy.apply(std::slice::from_ref(&edit), None) {
        Ok(_) => {
            assert!(
                expected.is_empty(),
                "{edit:?} applied while referred to by {expected:?}"
            );
            assert_no_dangling(&copy);
        }
        Err(ProtocolError::InvalidArgument { blockers, .. }) if !expected.is_empty() => {
            assert_eq!(&copy, graph, "a refused {edit:?} changed the graph");
            assert_eq!(&blocker_set(&blockers), expected, "blockers of {edit:?}");
        }
        Err(e) => panic!("{edit:?} with referrers {expected:?} failed with {e}"),
    }
}

/// Every scene, source, output and encoder profile delete, on copies of `graph`.
fn check_deletes(graph: &Graph) {
    let state = graph.state();
    for scene in state.scenes() {
        let target = Reference::Scene {
            scene_id: scene.id().clone(),
        };
        let edit = Edit::SceneDelete {
            scene_id: scene.id().clone(),
        };
        check_delete(graph, edit, &referrers(graph, &target));
    }
    for source in state.sources() {
        let target = Reference::Source {
            source_id: source.id().clone(),
        };
        let edit = Edit::SourceDelete {
            source_id: source.id().clone(),
        };
        check_delete(graph, edit, &referrers(graph, &target));
    }
    // Nothing blocks an output's delete (none runs here); it takes its bindings with it.
    for output in state.outputs() {
        let edit = Edit::OutputDelete {
            output_id: output.id().clone(),
        };
        check_delete(graph, edit, &Referrers::new());
    }
    for encoder in state.encoders() {
        let users: Vec<_> = state
            .outputs()
            .filter(|o| o.encoder_profile() == Some(encoder.id()))
            .map(|o| o.id().clone())
            .collect();
        let mut copy = graph.clone();
        let edit = Edit::EncoderDelete {
            profile_id: encoder.id().clone(),
        };
        match copy.apply(std::slice::from_ref(&edit), None) {
            Ok(_) => assert!(users.is_empty(), "{edit:?} applied while used by {users:?}"),
            Err(ProtocolError::InvalidArgument { .. }) if !users.is_empty() => {
                assert_eq!(&copy, graph, "a refused {edit:?} changed the graph");
            }
            Err(e) => panic!("{edit:?} with users {users:?} failed with {e}"),
        }
    }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn apply_then_inverse_restores_the_state(batches in batches()) {
        let mut graph = start();
        for edits in &batches {
            let before = graph.clone();
            match graph.apply(edits, Some(before.rev())) {
                Ok(applied) => {
                    prop_assert_eq!(applied.rev.0, before.rev().0 + 1);
                    let after = graph.clone();
                    let undo = graph
                        .apply(&applied.inverse.edits, Some(applied.rev))
                        .map_err(|e| TestCaseError::fail(format!("inverse of {edits:?}: {e}")))?;
                    prop_assert_eq!(graph.state(), before.state(), "undo of {:?}", edits);
                    graph
                        .apply(&undo.inverse.edits, Some(undo.rev))
                        .map_err(|e| TestCaseError::fail(format!("redo of {edits:?}: {e}")))?;
                    prop_assert_eq!(graph.state(), after.state(), "redo of {:?}", edits);
                }
                Err(_) => prop_assert_eq!(&graph, &before, "a failed {:?} changed the graph", edits),
            }
        }
    }

    #[test]
    fn deletes_are_refused_while_referenced(batches in batches()) {
        let mut graph = start();
        for edits in &batches {
            let _ = graph.apply(edits, None);
            assert_no_dangling(&graph);
            check_deletes(&graph);
        }
    }
}
