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
    AudioCodec, AudioEncoding, Batch, Canvas, Crop, Edit, EncoderProfile, EncoderProfileId,
    EncoderProfileInfo, EntityKind, Finite32, FitMode, HardwarePreference, ItemGeometry, ItemId,
    ItemInfo, ItemTransform, KindCategory, KindDescriptor, OutputId, OutputInfo, OutputKind,
    PlacementForm, ProtocolError, Query, QueryResult, RateControl, RecordContainer, Rev,
    RtmpTarget, SceneId, SceneInfo, SceneRole, SceneSummary, Size, SourceId, SourceInfo,
    SourceKind, Vec2, VideoCodec, VideoEncoding,
};
use crate::{AudioFollow, BusId, BusInfo, SendInfo, SourceAudio};

/// The revision of a collection that started empty at rev 0 once [`collection`] has been
/// applied to it: the batch is applied as one batch, so exactly one revision later.
pub const COLLECTION_REV: Rev = Rev(1);

/// The kinds an implementation must describe for [`collection`] to apply: `webcam`, a
/// video kind, and `mic`, an audio kind with no video, both without parameters, so
/// every implementation reports their settings as an empty map.
pub fn kinds() -> Vec<KindDescriptor> {
    vec![
        KindDescriptor {
            kind_id: "mic".into(),
            category: KindCategory::Source,
            name: "Microphone".into(),
            version: 1,
            video: false,
            audio: true,
            params: vec![],
        },
        KindDescriptor {
            kind_id: "webcam".into(),
            category: KindCategory::Source,
            name: "Webcam".into(),
            version: 1,
            video: true,
            audio: false,
            params: vec![],
        },
    ]
}

/// The batch that builds the fixture collection from an empty one: a 1280×720 canvas,
/// scenes `b` then `a` (created `a` first, then reordered), source `cam` placed twice in
/// `a` (the second placement moved, cropped, hidden and reordered to the bottom) and once
/// in `b` with a geometry, scene `b` nested in `a` through the scene source `b_src`, the
/// RTMP outputs `yt` and `tw` (`tw`'s target changed after it was created), the
/// recording output `rec`, the encoder profiles `hq` (updated after it was created,
/// used by `rec`) and `low` (used by `yt`, `tw` keeping the engine's default), and the
/// audio: the `mic` source and its copy `mic_copy`, the bus `vod` beside the master
/// (`drums` was created with a send and deleted again), sends and faders changed after
/// they were made, and `b_src`'s audio turned off. Then the
/// references and clones of ADR-0023: `cam` is duplicated as `cam_copy` and `i1` is
/// switched to place the copy, keeping its ID and position; `aux` is created with a
/// `duplicated_from` naming a source that doesn't exist, which reads as absent.
///
/// Sources, outputs and encoder profiles are created in the opposite order of their IDs, so reads that
/// order them by ID can't pass by echoing creation order.
pub fn collection() -> Batch {
    let source = |s: SourceInfo| Edit::SourceCreate {
        source_id: s.source_id,
        name: s.name,
        kind: s.kind,
        settings: s.settings,
        duplicated_from: s.duplicated_from,
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
            },
            Edit::SceneCreate {
                scene_id: SceneId::new("b"),
                name: "B".into(),
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
            Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("low"),
                name: low().name,
                profile: low().profile,
            },
            Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("hq"),
                name: "Draft".into(),
                profile: EncoderProfile::DEFAULT,
            },
            Edit::EncoderUpdate {
                profile_id: EncoderProfileId::new("hq"),
                name: hq().name,
                profile: hq().profile,
            },
            Edit::OutputSetEncoder {
                output_id: OutputId::new("rec"),
                profile_id: Some(EncoderProfileId::new("hq")),
            },
            Edit::OutputSetEncoder {
                output_id: OutputId::new("yt"),
                profile_id: Some(EncoderProfileId::new("low")),
            },
            Edit::OutputSetEncoder {
                output_id: OutputId::new("tw"),
                profile_id: Some(EncoderProfileId::new("low")),
            },
            Edit::OutputSetEncoder {
                output_id: OutputId::new("tw"),
                profile_id: None,
            },
            Edit::SourceDuplicate {
                source_id: SourceId::new("cam"),
                new_source_id: SourceId::new("cam_copy"),
                name: "Camera copy".into(),
            },
            Edit::ItemSetSource {
                item_id: ItemId::new("i1"),
                source_id: SourceId::new("cam_copy"),
            },
            source(SourceInfo {
                duplicated_from: Some(SourceId::new("gone")),
                ..aux()
            }),
            source(mic()),
            Edit::BusCreate {
                bus_id: BusId::new("vod"),
                name: "VOD".into(),
            },
            Edit::BusCreate {
                bus_id: BusId::new("drums"),
                name: "Drums".into(),
            },
            Edit::SendSet {
                source_id: SourceId::new("mic"),
                bus_id: BusId::new("drums"),
                gain_db: finite(-12.0),
            },
            Edit::BusDelete {
                bus_id: BusId::new("drums"),
            },
            Edit::BusRename {
                bus_id: BusId::new("vod"),
                name: "VOD track".into(),
            },
            Edit::BusSetFader {
                bus_id: BusId::new("vod"),
                gain_db: Some(finite(-6.0)),
                muted: Some(true),
            },
            Edit::BusSetFader {
                bus_id: BusId::master(),
                gain_db: Some(finite(-1.0)),
                muted: None,
            },
            Edit::SendSet {
                source_id: SourceId::new("mic"),
                bus_id: BusId::new("vod"),
                gain_db: finite(-3.0),
            },
            Edit::SendSet {
                source_id: SourceId::new("mic"),
                bus_id: BusId::master(),
                gain_db: finite(-1.5),
            },
            Edit::SourceSetAudio {
                source_id: SourceId::new("mic"),
                enabled: None,
                follow: Some(AudioFollow::WhenShown),
                gain_db: Some(finite(-2.0)),
                muted: Some(true),
            },
            Edit::SourceDuplicate {
                source_id: SourceId::new("mic"),
                new_source_id: SourceId::new("mic_copy"),
                name: "Microphone copy".into(),
            },
            Edit::SendRemove {
                source_id: SourceId::new("mic_copy"),
                bus_id: BusId::master(),
            },
            Edit::SourceSetAudio {
                source_id: SourceId::new("b_src"),
                enabled: Some(false),
                follow: None,
                gain_db: None,
                muted: None,
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
                        role: SceneRole::Scene,
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
                sources: sources(),
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
                sources: sources(),
                scenes: vec![
                    SceneInfo {
                        scene_id: scene("b"),
                        name: "B".into(),
                        role: SceneRole::Scene,
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
                encoders: encoders(),
                buses: buses(),
                sends: sends(),
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
        // Encoder profiles by ID, too.
        (
            Query::EncoderList,
            Ok(QueryResult::EncoderList {
                rev: COLLECTION_REV,
                encoders: encoders(),
            }),
        ),
        // Buses by ID, the master among them; sends by bus, then by source.
        (
            Query::BusList,
            Ok(QueryResult::BusList {
                rev: COLLECTION_REV,
                buses: buses(),
                sends: sends(),
            }),
        ),
    ]
}

/// Batches every implementation rejects once [`collection`] has been applied, each sent
/// alone against that collection, with the exact error. A rejected batch applies
/// nothing, so afterwards the collection is still at [`COLLECTION_REV`] and [`reads`]
/// still holds.
pub fn rejected() -> Vec<(Batch, ProtocolError)> {
    let invalid = |field: &str, reason: &str| ProtocolError::invalid_argument(field, reason);
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
            }),
            invalid("edits.0.scene_id", "must not be empty"),
        ),
        (
            Batch::single(Edit::SceneCreate {
                scene_id: SceneId::new("a"),
                name: "Again".into(),
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
            Batch::single(Edit::ItemSetSource {
                item_id: ItemId::new("ghost"),
                source_id: SourceId::new("cam"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Item,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::ItemSetSource {
                item_id: ItemId::new("i1"),
                source_id: SourceId::new("ghost"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Source,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::SourceCreate {
                source_id: SourceId::new("loop"),
                name: "Loop".into(),
                kind: cam().kind,
                settings: crate::Settings::new(),
                duplicated_from: Some(SourceId::new("loop")),
            }),
            invalid("edits.0.duplicated_from", "must not name the source itself"),
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
        (
            Batch::single(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("hq"),
                name: "Again".into(),
                profile: EncoderProfile::DEFAULT,
            }),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Encoder,
                id: "hq".into(),
            },
        ),
        (
            Batch::single(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new(""),
                name: "Empty".into(),
                profile: EncoderProfile::DEFAULT,
            }),
            invalid("edits.0.profile_id", "must not be empty"),
        ),
        (
            Batch::single(Edit::EncoderUpdate {
                profile_id: EncoderProfileId::new("ghost"),
                name: "Ghost".into(),
                profile: EncoderProfile::DEFAULT,
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Encoder,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("new"),
                name: "Slow".into(),
                profile: with_video_kbps(50),
            }),
            invalid(
                "edits.0.profile.video.rate_control.kbps",
                "must be in 100..=100000",
            ),
        ),
        (
            Batch::single(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("new"),
                name: "Peak".into(),
                profile: EncoderProfile {
                    video: VideoEncoding {
                        rate_control: RateControl::Vbr {
                            kbps: 6000,
                            max_kbps: 4000,
                        },
                        ..EncoderProfile::DEFAULT.video
                    },
                    ..EncoderProfile::DEFAULT
                },
            }),
            invalid(
                "edits.0.profile.video.rate_control.max_kbps",
                "must be at least `kbps` and at most 200000",
            ),
        ),
        (
            Batch::single(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("new"),
                name: "Keyframes".into(),
                profile: EncoderProfile {
                    video: VideoEncoding {
                        keyframe_interval_s: 0,
                        ..EncoderProfile::DEFAULT.video
                    },
                    ..EncoderProfile::DEFAULT
                },
            }),
            invalid(
                "edits.0.profile.video.keyframe_interval_s",
                "must be in 1..=10",
            ),
        ),
        (
            Batch::single(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("new"),
                name: "Loud".into(),
                profile: EncoderProfile {
                    audio: AudioEncoding {
                        codec: AudioCodec::Aac,
                        kbps: 512,
                    },
                    ..EncoderProfile::DEFAULT
                },
            }),
            invalid("edits.0.profile.audio.kbps", "must be in 32..=320"),
        ),
        (
            Batch::single(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("new"),
                name: "Later".into(),
                profile: EncoderProfile {
                    video: VideoEncoding {
                        codec: VideoCodec::Unknown,
                        ..EncoderProfile::DEFAULT.video
                    },
                    ..EncoderProfile::DEFAULT
                },
            }),
            invalid(
                "edits.0.profile.video.codec",
                "is not a video codec this engine knows",
            ),
        ),
        (
            Batch::single(Edit::OutputSetEncoder {
                output_id: OutputId::new("yt"),
                profile_id: Some(EncoderProfileId::new("ghost")),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Encoder,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::OutputSetEncoder {
                output_id: OutputId::new("ghost"),
                profile_id: None,
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Output,
                id: "ghost".into(),
            },
        ),
        // A profile an output uses can't be deleted, until the output moves off it.
        (
            Batch::single(Edit::EncoderDelete {
                profile_id: EncoderProfileId::new("hq"),
            }),
            invalid(
                "edits.0.profile_id",
                "encoder profile `hq` is used by output `rec`; point it elsewhere first",
            ),
        ),
        (
            Batch::single(Edit::EncoderDelete {
                profile_id: EncoderProfileId::new("ghost"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Encoder,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::BusCreate {
                bus_id: BusId::master(),
                name: "Again".into(),
            }),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Bus,
                id: "master".into(),
            },
        ),
        (
            Batch::single(Edit::BusCreate {
                bus_id: BusId::new(""),
                name: "Empty".into(),
            }),
            invalid("edits.0.bus_id", "must not be empty"),
        ),
        // 14 more buses fit beside `master` and `vod`; the 15th is one too many.
        (
            Batch {
                edits: (0..15)
                    .map(|n| Edit::BusCreate {
                        bus_id: BusId::new(format!("extra{n}")),
                        name: format!("Extra {n}"),
                    })
                    .collect(),
                expected_rev: None,
                idempotency_key: None,
            },
            invalid(
                "edits.14.bus_id",
                "a collection holds at most 16 buses, the master included",
            ),
        ),
        (
            Batch::single(Edit::BusDelete {
                bus_id: BusId::master(),
            }),
            invalid("edits.0.bus_id", "the master bus can't be deleted"),
        ),
        (
            Batch::single(Edit::BusDelete {
                bus_id: BusId::new("ghost"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Bus,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::BusSetFader {
                bus_id: BusId::new("vod"),
                gain_db: None,
                muted: None,
            }),
            invalid("edits.0.gain_db", "send `gain_db`, `muted` or both"),
        ),
        (
            Batch::single(Edit::BusSetFader {
                bus_id: BusId::new("vod"),
                gain_db: Some(finite(30.0)),
                muted: None,
            }),
            invalid("edits.0.gain_db", "must be at most +24 dB"),
        ),
        (
            Batch::single(Edit::SourceSetAudio {
                source_id: SourceId::new("cam"),
                enabled: Some(false),
                follow: None,
                gain_db: None,
                muted: None,
            }),
            invalid("edits.0.source_id", "source `cam` has no audio"),
        ),
        (
            Batch::single(Edit::SourceSetAudio {
                source_id: SourceId::new("mic"),
                enabled: None,
                follow: None,
                gain_db: None,
                muted: None,
            }),
            invalid(
                "edits.0.enabled",
                "send at least one of `enabled`, `follow`, `gain_db` and `muted`",
            ),
        ),
        (
            Batch::single(Edit::SourceSetAudio {
                source_id: SourceId::new("b_src"),
                enabled: None,
                follow: None,
                gain_db: None,
                muted: Some(true),
            }),
            invalid(
                "edits.0.muted",
                "a `scene` source has no fader or mute of its own",
            ),
        ),
        (
            Batch::single(Edit::SourceSetAudio {
                source_id: SourceId::new("mic"),
                enabled: None,
                follow: None,
                gain_db: Some(finite(-120.0)),
                muted: None,
            }),
            invalid(
                "edits.0.gain_db",
                "must be at least -100 dB; mute for silence",
            ),
        ),
        (
            Batch::single(Edit::SendSet {
                source_id: SourceId::new("b_src"),
                bus_id: BusId::new("vod"),
                gain_db: Finite32::ZERO,
            }),
            invalid(
                "edits.0.source_id",
                "a `scene` source has no sends of its own; what it shows sends",
            ),
        ),
        (
            Batch::single(Edit::SendSet {
                source_id: SourceId::new("cam"),
                bus_id: BusId::new("vod"),
                gain_db: Finite32::ZERO,
            }),
            invalid("edits.0.source_id", "source `cam` has no audio"),
        ),
        (
            Batch::single(Edit::SendSet {
                source_id: SourceId::new("mic"),
                bus_id: BusId::new("ghost"),
                gain_db: Finite32::ZERO,
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Bus,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::SendRemove {
                source_id: SourceId::new("mic_copy"),
                bus_id: BusId::master(),
            }),
            invalid(
                "edits.0.bus_id",
                "source `mic_copy` has no send to bus `master`",
            ),
        ),
    ]
}

/// The collection's buses, ordered by ID.
fn buses() -> Vec<BusInfo> {
    vec![
        BusInfo {
            bus_id: BusId::master(),
            name: "Master".into(),
            gain_db: finite(-1.0),
            muted: false,
        },
        BusInfo {
            bus_id: BusId::new("vod"),
            name: "VOD track".into(),
            gain_db: finite(-6.0),
            muted: true,
        },
    ]
}

/// The collection's sends, ordered by bus, then by source: the copy keeps `mic`'s send
/// into `vod`, and lost its master send.
fn sends() -> Vec<SendInfo> {
    let send = |source: &str, bus: BusId, gain_db: f32| SendInfo {
        source_id: SourceId::new(source),
        bus_id: bus,
        gain_db: finite(gain_db),
    };
    vec![
        send("mic", BusId::master(), -1.5),
        send("mic", BusId::new("vod"), -3.0),
        send("mic_copy", BusId::new("vod"), -3.0),
    ]
}

/// Profile `low`: 3000 kbit/s CBR, a keyframe every 2 s, hardware preferred, AAC at
/// 128 kbit/s.
fn low() -> EncoderProfileInfo {
    EncoderProfileInfo {
        profile_id: EncoderProfileId::new("low"),
        name: "Stream".into(),
        profile: EncoderProfile {
            video: VideoEncoding {
                codec: VideoCodec::H264,
                rate_control: RateControl::Cbr { kbps: 3000 },
                keyframe_interval_s: 2,
                hardware: HardwarePreference::Prefer,
            },
            audio: AudioEncoding {
                codec: AudioCodec::Aac,
                kbps: 128,
            },
        },
    }
}

/// Profile `hq`, as `encoder.update` left it: 12000 kbit/s VBR peaking at 20000, a
/// keyframe every 4 s, AAC at 320 kbit/s.
fn hq() -> EncoderProfileInfo {
    EncoderProfileInfo {
        profile_id: EncoderProfileId::new("hq"),
        name: "Recording".into(),
        profile: EncoderProfile {
            video: VideoEncoding {
                codec: VideoCodec::H264,
                rate_control: RateControl::Vbr {
                    kbps: 12000,
                    max_kbps: 20000,
                },
                keyframe_interval_s: 4,
                hardware: HardwarePreference::Avoid,
            },
            audio: AudioEncoding {
                codec: AudioCodec::Aac,
                kbps: 320,
            },
        },
    }
}

/// The collection's encoder profiles, ordered by ID.
fn encoders() -> Vec<EncoderProfileInfo> {
    vec![hq(), low()]
}

/// The default profile at another constant video bit rate.
fn with_video_kbps(kbps: u32) -> EncoderProfile {
    EncoderProfile {
        video: VideoEncoding {
            rate_control: RateControl::Cbr { kbps },
            ..EncoderProfile::DEFAULT.video
        },
        ..EncoderProfile::DEFAULT
    }
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
        encoder_profile: Some(EncoderProfileId::new("hq")),
    }
}

fn youtube() -> OutputInfo {
    OutputInfo {
        output_id: OutputId::new("yt"),
        kind: rtmp("rtmps://a.rtmps.youtube.com:443/live2"),
        encoder_profile: Some(EncoderProfileId::new("low")),
    }
}

/// The collection's outputs, ordered by ID.
fn outputs() -> Vec<OutputInfo> {
    vec![
        recording(),
        OutputInfo {
            output_id: OutputId::new("tw"),
            kind: rtmp(TWITCH_URL),
            encoder_profile: None,
        },
        youtube(),
    ]
}

/// The collection's sources, ordered by ID, as the edits left them.
fn sources() -> Vec<SourceInfo> {
    let mic_audio = SourceAudio {
        gain_db: finite(-2.0),
        muted: true,
        ..SourceAudio::new(AudioFollow::WhenShown)
    };
    vec![
        aux(),
        SourceInfo {
            audio: Some(SourceAudio {
                enabled: false,
                ..SourceAudio::new(AudioFollow::WhenShown)
            }),
            ..nested_b()
        },
        cam(),
        SourceInfo {
            source_id: SourceId::new("cam_copy"),
            name: "Camera copy".into(),
            duplicated_from: Some(SourceId::new("cam")),
            ..cam()
        },
        SourceInfo {
            audio: Some(mic_audio),
            ..mic()
        },
        SourceInfo {
            source_id: SourceId::new("mic_copy"),
            name: "Microphone copy".into(),
            duplicated_from: Some(SourceId::new("mic")),
            audio: Some(mic_audio),
            ..mic()
        },
    ]
}

/// An audio input as `source.create` makes it: an audio kind with no video follows
/// `always`.
fn mic() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("mic"),
        name: "Microphone".into(),
        kind: SourceKind::Builtin { kind: "mic".into() },
        settings: crate::Settings::new(),
        duplicated_from: None,
        audio: Some(SourceAudio::new(AudioFollow::Always)),
    }
}

/// Created with a `duplicated_from` naming a source that doesn't exist, so it reads as
/// absent.
fn aux() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("aux"),
        name: "Aux".into(),
        ..cam()
    }
}

fn cam() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("cam"),
        name: "Camera".into(),
        kind: SourceKind::Builtin {
            kind: "webcam".into(),
        },
        settings: crate::Settings::new(),
        duplicated_from: None,
        audio: None,
    }
}

/// A `scene` source as `source.create` makes it: its audio follows `when_shown`.
fn nested_b() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("b_src"),
        name: "Scene B".into(),
        kind: SourceKind::Scene {
            scene_id: SceneId::new("b"),
        },
        settings: crate::Settings::new(),
        duplicated_from: None,
        audio: Some(SourceAudio::new(AudioFollow::WhenShown)),
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
        plain("i1", "cam_copy"),
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
