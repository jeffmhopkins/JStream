//! A shared parity fixture: one collection, built by one batch, and what every
//! implementation of the protocol must answer about it. Behind the non-default
//! `fixtures` feature.
//!
//! `jstream-graph` (the engine's model) and `jstream-control`'s mock engine both run
//! these cases in their tests. They sit in the same layer and can't depend on each other
//! (R-ARCH-002), so this fixture is the one place both read the expected answers from:
//! if either implementation drifts from it, its own test fails.
//!
//! Docs: crates/jstream-protocol/README.md#purpose

use crate::{
    Batch, Canvas, Crop, Edit, EntityKind, Finite32, FitMode, ItemGeometry, ItemId, ItemInfo,
    ItemTransform, KindCategory, KindDescriptor, OutputId, OutputInfo, OutputKind, PlacementForm,
    ProtocolError, Query, QueryResult, RecordContainer, Rev, RtmpTarget, SceneId, SceneInfo,
    SceneRole, SceneSummary, Size, SourceId, SourceInfo, SourceKind, Vec2,
};

/// The revision of a collection that started empty at rev 0 once [`collection`] has been
/// applied to it: the batch is applied as one batch, so exactly one revision later.
pub const COLLECTION_REV: Rev = Rev(1);

/// The kinds an implementation must describe for [`collection`] to apply: `webcam`, a
/// source kind without parameters, so every implementation reports its settings as an
/// empty map.
pub fn kinds() -> Vec<KindDescriptor> {
    vec![KindDescriptor {
        kind_id: "webcam".into(),
        category: KindCategory::Source,
        name: "Webcam".into(),
        version: 1,
        video: true,
        audio: false,
        params: vec![],
    }]
}

/// The batch that builds the fixture collection from an empty one: a 1280×720 canvas,
/// scenes `b` then `a` (created `a` first, then reordered), source `cam` placed twice in
/// `a` (the second placement moved, cropped, hidden and reordered to the bottom) and once
/// in `b` with a geometry, scene `b` nested in `a` through the scene source `b_src`, the
/// RTMP outputs `yt` and `tw` (`tw`'s target changed after it was created), and the
/// recording output `rec`.
///
/// Sources and outputs are created in the opposite order of their IDs, so reads that
/// order them by ID can't pass by echoing creation order.
pub fn collection() -> Batch {
    let source = |s: SourceInfo| Edit::SourceCreate {
        source_id: s.source_id,
        name: s.name,
        kind: s.kind,
        settings: s.settings,
    };
    let add = |item: &str, source: &str| Edit::ItemAdd {
        scene_id: SceneId::new("a"),
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
        transform: None,
        geometry: None,
    };
    Batch {
        edits: vec![
            Edit::CanvasSetSize {
                width: CANVAS.width,
                height: CANVAS.height,
            },
            Edit::SceneCreate {
                scene_id: SceneId::new("a"),
                name: "A".into(),
                role: None,
            },
            // `b` is placed in `a` through `b_src`, so it is created as a group.
            Edit::SceneCreate {
                scene_id: SceneId::new("b"),
                name: "B".into(),
                role: Some(SceneRole::Group),
            },
            source(nested_b()),
            source(cam()),
            add("i1", "cam"),
            add("i2", "b_src"),
            add("i3", "cam"),
            Edit::ItemSetTransform {
                item_id: ItemId::new("i3"),
                scene_id: None,
                transform: MOVED,
            },
            Edit::ItemSetCrop {
                item_id: ItemId::new("i3"),
                scene_id: None,
                crop: CROP,
            },
            Edit::ItemSetVisible {
                item_id: ItemId::new("i3"),
                scene_id: None,
                visible: false,
            },
            Edit::ItemReorder {
                item_id: ItemId::new("i3"),
                index: 0,
            },
            Edit::SceneReorder {
                scene_id: SceneId::new("b"),
                index: 0,
            },
            Edit::ItemAdd {
                scene_id: SceneId::new("b"),
                item_id: ItemId::new("i4"),
                source_id: SourceId::new("cam"),
                transform: None,
                geometry: Some(BOXED),
            },
            Edit::OutputCreate {
                output_id: OutputId::new("yt"),
                kind: youtube().kind,
            },
            Edit::OutputCreate {
                output_id: OutputId::new("tw"),
                kind: rtmp("rtmp://old.example.com/app"),
            },
            Edit::OutputSetTarget {
                output_id: OutputId::new("tw"),
                target: RtmpTarget::Generic {
                    url: TWITCH_URL.into(),
                },
            },
            Edit::OutputCreate {
                output_id: OutputId::new("rec"),
                kind: recording().kind,
            },
        ],
        expected_rev: Some(Rev(0)),
        idempotency_key: None,
    }
}

/// Every collection read, and its answer at [`COLLECTION_REV`] once [`collection`] has
/// been applied to an empty collection. Reads change nothing, so the cases may run in any
/// order.
pub fn reads() -> Vec<(Query, Result<QueryResult, ProtocolError>)> {
    let scene = |id: &str| SceneId::new(id);
    vec![
        (
            Query::SceneList,
            Ok(QueryResult::SceneList {
                rev: COLLECTION_REV,
                scenes: vec![
                    SceneSummary {
                        scene_id: scene("b"),
                        name: "B".into(),
                        role: SceneRole::Group,
                        item_count: 1,
                    },
                    SceneSummary {
                        scene_id: scene("a"),
                        name: "A".into(),
                        role: SceneRole::Scene,
                        item_count: 3,
                    },
                ],
            }),
        ),
        // Sources by ID, whatever order they were created in.
        (
            Query::SourceList,
            Ok(QueryResult::SourceList {
                rev: COLLECTION_REV,
                sources: vec![nested_b(), cam()],
            }),
        ),
        (
            Query::ItemList {
                scene_id: scene("a"),
            },
            Ok(QueryResult::ItemList {
                rev: COLLECTION_REV,
                scene_id: scene("a"),
                items: items_of_a(),
            }),
        ),
        (
            Query::ItemList {
                scene_id: scene("b"),
            },
            Ok(QueryResult::ItemList {
                rev: COLLECTION_REV,
                scene_id: scene("b"),
                items: items_of_b(),
            }),
        ),
        (
            Query::ItemList {
                scene_id: scene("ghost"),
            },
            Err(ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "ghost".into(),
            }),
        ),
        (
            Query::StateGet,
            Ok(QueryResult::StateGet {
                rev: COLLECTION_REV,
                canvas: CANVAS,
                sources: vec![nested_b(), cam()],
                scenes: vec![
                    SceneInfo {
                        scene_id: scene("b"),
                        name: "B".into(),
                        role: SceneRole::Group,
                        items: items_of_b(),
                    },
                    SceneInfo {
                        scene_id: scene("a"),
                        name: "A".into(),
                        role: SceneRole::Scene,
                        items: items_of_a(),
                    },
                ],
                outputs: outputs(),
            }),
        ),
        // Outputs by ID, whatever order they were created in.
        (
            Query::OutputList,
            Ok(QueryResult::OutputList {
                rev: COLLECTION_REV,
                outputs: outputs(),
            }),
        ),
    ]
}

/// Batches every implementation rejects once [`collection`] has been applied, each sent
/// alone against that collection, with the exact error. A rejected batch applies
/// nothing, so afterwards the collection is still at [`COLLECTION_REV`] and [`reads`]
/// still holds.
pub fn rejected() -> Vec<(Batch, ProtocolError)> {
    let invalid = |field: &str, reason: &str| ProtocolError::InvalidArgument {
        field: field.into(),
        reason: reason.into(),
    };
    vec![
        // The revision check comes before the empty-batch check.
        (
            Batch {
                edits: vec![],
                expected_rev: Some(Rev(0)),
                idempotency_key: None,
            },
            ProtocolError::RevConflict {
                expected: Rev(0),
                current: COLLECTION_REV,
            },
        ),
        (
            Batch {
                edits: vec![],
                expected_rev: None,
                idempotency_key: None,
            },
            ProtocolError::EmptyBatch,
        ),
        (
            Batch::single(Edit::SceneCreate {
                scene_id: SceneId::new(""),
                name: String::new(),
                role: None,
            }),
            invalid("edits.0.scene_id", "must not be empty"),
        ),
        (
            Batch::single(Edit::SceneCreate {
                scene_id: SceneId::new("a"),
                name: "Again".into(),
                role: None,
            }),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Scene,
                id: "a".into(),
            },
        ),
        (
            Batch::single(Edit::ItemAdd {
                scene_id: SceneId::new("a"),
                item_id: ItemId::new("i9"),
                source_id: SourceId::new("ghost"),
                transform: None,
                geometry: None,
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Source,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::ItemReorder {
                item_id: ItemId::new("i1"),
                index: 3,
            }),
            invalid(
                "edits.0.index",
                "must be less than the number of items in the scene (3)",
            ),
        ),
        (
            Batch::single(Edit::ItemAdd {
                scene_id: SceneId::new("a"),
                item_id: ItemId::new("i9"),
                source_id: SourceId::new("cam"),
                transform: Some(ItemTransform::IDENTITY),
                geometry: Some(BOXED),
            }),
            invalid(
                "edits.0.geometry",
                "send either `transform` or `geometry`, not both",
            ),
        ),
        (
            Batch::single(Edit::ItemSetGeometry {
                item_id: ItemId::new("i1"),
                scene_id: None,
                geometry: ItemGeometry {
                    tilt_deg: finite(10.0),
                    ..BOXED
                },
            }),
            invalid(
                "edits.0.geometry.tilt_deg",
                "tilt, turn, depth and perspective are not drawn yet, so they must be neutral",
            ),
        ),
        (
            Batch::single(Edit::ItemSetGeometry {
                item_id: ItemId::new("i1"),
                scene_id: None,
                geometry: ItemGeometry {
                    size: Size {
                        width: finite(0.0),
                        height: finite(10.0),
                    },
                    ..BOXED
                },
            }),
            invalid(
                "edits.0.geometry.size",
                "item size must be greater than zero",
            ),
        ),
        (
            Batch::single(Edit::CanvasSetSize {
                width: 0,
                height: 720,
            }),
            invalid("edits.0.width", "must be from 1 to 8192"),
        ),
        // The first edit applies inside the batch, the second fails, and the whole batch
        // is rolled back.
        (
            Batch {
                edits: vec![
                    Edit::SceneRename {
                        scene_id: SceneId::new("a"),
                        name: "Renamed".into(),
                    },
                    Edit::ItemRemove {
                        item_id: ItemId::new("ghost"),
                    },
                ],
                expected_rev: Some(COLLECTION_REV),
                idempotency_key: None,
            },
            ProtocolError::NotFound {
                entity: EntityKind::Item,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new(""),
                kind: rtmp("rtmp://host/app"),
            }),
            invalid("edits.0.output_id", "must not be empty"),
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("yt"),
                kind: rtmp("rtmp://host/app"),
            }),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Output,
                id: "yt".into(),
            },
        ),
        // URLs that could carry a credential are refused, and the reason never repeats
        // the URL.
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: rtmp("rtmp://user:secret@host/app"),
            }),
            invalid(
                "edits.0.kind.target.url",
                "must not carry credentials (`user:password@`): set the stream key with \
                 `output.set_key`",
            ),
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: rtmp("rtmp://host/app?key=secret"),
            }),
            invalid(
                "edits.0.kind.target.url",
                "must not have a query or fragment: set the stream key with `output.set_key`",
            ),
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: rtmp("srt://host/app"),
            }),
            invalid(
                "edits.0.kind.target.url",
                "the scheme must be `rtmp` or `rtmps`",
            ),
        ),
        (
            Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("ghost"),
                target: RtmpTarget::Generic {
                    url: "rtmp://host/app".into(),
                },
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Output,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("yt"),
                target: RtmpTarget::Generic {
                    url: "rtmp://host:99999/app".into(),
                },
            }),
            invalid(
                "edits.0.target.url",
                "has a port that is not a number in 1..=65535",
            ),
        ),
        (
            Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("yt"),
                target: RtmpTarget::Generic {
                    url: "rtmp://host/".into(),
                },
            }),
            invalid(
                "edits.0.target.url",
                "is missing the application name (the path after the host)",
            ),
        ),
        // Recording paths are checked the same way on every platform.
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: record("Videos/show.mp4"),
            }),
            invalid(
                "edits.0.kind.path",
                "must be absolute: `/…`, `\\…`, `C:\\…` or `C:/…`",
            ),
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: record("C:\\Videos\\..\\show.mp4"),
            }),
            invalid("edits.0.kind.path", "must not contain a `..` segment"),
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: record("/home/me/Videos/"),
            }),
            invalid("edits.0.kind.path", "must end in a file name"),
        ),
        (
            Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("rec"),
                target: RtmpTarget::Generic {
                    url: "rtmp://host/app".into(),
                },
            }),
            invalid("edits.0.output_id", "the output is not an RTMP output"),
        ),
        // A delete inside the batch applies, the second fails, and the output is back.
        (
            Batch {
                edits: vec![
                    Edit::OutputDelete {
                        output_id: OutputId::new("yt"),
                    },
                    Edit::OutputDelete {
                        output_id: OutputId::new("yt"),
                    },
                ],
                expected_rev: None,
                idempotency_key: None,
            },
            ProtocolError::NotFound {
                entity: EntityKind::Output,
                id: "yt".into(),
            },
        ),
    ]
}

/// The server URL output `tw` ends up with.
const TWITCH_URL: &str = "rtmp://live.twitch.tv/app";

fn rtmp(url: &str) -> OutputKind {
    OutputKind::Rtmp {
        target: RtmpTarget::Generic { url: url.into() },
    }
}

fn record(path: &str) -> OutputKind {
    OutputKind::Record {
        path: path.into(),
        container: RecordContainer::Mp4,
    }
}

fn recording() -> OutputInfo {
    OutputInfo {
        output_id: OutputId::new("rec"),
        kind: record("/home/me/Videos/show.mp4"),
    }
}

fn youtube() -> OutputInfo {
    OutputInfo {
        output_id: OutputId::new("yt"),
        kind: rtmp("rtmps://a.rtmps.youtube.com:443/live2"),
    }
}

/// The collection's outputs, ordered by ID.
fn outputs() -> Vec<OutputInfo> {
    vec![
        recording(),
        OutputInfo {
            output_id: OutputId::new("tw"),
            kind: rtmp(TWITCH_URL),
        },
        youtube(),
    ]
}

fn cam() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("cam"),
        name: "Camera".into(),
        kind: SourceKind::Builtin {
            kind: "webcam".into(),
        },
        settings: crate::Settings::new(),
    }
}

fn nested_b() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("b_src"),
        name: "Scene B".into(),
        kind: SourceKind::Scene {
            scene_id: SceneId::new("b"),
        },
        settings: crate::Settings::new(),
    }
}

/// A finite number, checked at compile time (it is only used in constants), so the fixture never panics at run
/// time.
const fn finite(value: f32) -> Finite32 {
    match Finite32::new(value) {
        Some(finite) => finite,
        None => panic!("not finite"),
    }
}

/// Item `i3`'s transform: moved to (10, 20), otherwise the identity.
const MOVED: ItemTransform = ItemTransform {
    position: Vec2 {
        x: finite(10.0),
        y: finite(20.0),
    },
    ..ItemTransform::IDENTITY
};

/// The fixture's canvas, set by the first edit of [`collection`].
pub const CANVAS: Canvas = Canvas {
    width: 1280,
    height: 720,
};

/// Item `i4`'s geometry: a 320×180 box centred at (640, 360), filled.
const BOXED: ItemGeometry = ItemGeometry {
    center: Vec2 {
        x: finite(640.0),
        y: finite(360.0),
    },
    size: Size {
        width: finite(320.0),
        height: finite(180.0),
    },
    fit_mode: FitMode::Fill,
    rotation_deg: Finite32::ZERO,
    tilt_deg: Finite32::ZERO,
    turn_deg: Finite32::ZERO,
    depth_px: Finite32::ZERO,
    perspective_px: None,
};

/// `ItemInfo.transform` for [`BOXED`]: its unrotated top-left corner at scale (1, 1).
const BOXED_VIEW: ItemTransform = ItemTransform {
    position: Vec2 {
        x: finite(480.0),
        y: finite(270.0),
    },
    ..ItemTransform::IDENTITY
};

const CROP: Crop = Crop {
    left: 1,
    top: 2,
    right: 3,
    bottom: 4,
};

/// Scene `a`'s items, bottom first.
fn items_of_a() -> Vec<ItemInfo> {
    let plain = |item: &str, source: &str| ItemInfo {
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
        placement: PlacementForm::Legacy,
        transform: ItemTransform::IDENTITY,
        geometry: None,
        crop: Crop::NONE,
        visible: true,
    };
    vec![
        ItemInfo {
            item_id: ItemId::new("i3"),
            source_id: SourceId::new("cam"),
            placement: PlacementForm::Legacy,
            transform: MOVED,
            geometry: None,
            crop: CROP,
            visible: false,
        },
        plain("i1", "cam"),
        plain("i2", "b_src"),
    ]
}

/// Scene `b`'s one item, placed with a geometry.
fn items_of_b() -> Vec<ItemInfo> {
    vec![ItemInfo {
        item_id: ItemId::new("i4"),
        source_id: SourceId::new("cam"),
        placement: PlacementForm::Geometry,
        transform: BOXED_VIEW,
        geometry: Some(BOXED),
        crop: Crop::NONE,
        visible: true,
    }]
}
