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

use crate::ItemTransition;
use crate::{
    Action, AudioCodec, AudioEncoding, Batch, BlendMode, Canvas, Chord, Crop, Edit, EncoderProfile,
    EncoderProfileId, EncoderProfileInfo, EntityKind, FilterChain, FilterChains, FilterId,
    FilterInfo, FilterOwner, Finite32, FitMode, HardwarePreference, HotkeyBinding, ItemGeometry,
    ItemId, ItemInfo, ItemTransform, KindCategory, KindDescriptor, MasterInfo, OutputId,
    OutputInfo, OutputKind, ParamDescriptor, ParamType, PlacementForm, ProtocolError, Query,
    QueryResult, RateControl, RecordContainer, Rev, RtmpTarget, SceneId, SceneInfo, SceneRole,
    SceneSummary, Size, SourceId, SourceInfo, SourceKind, TransitionsInfo, Vec2, VideoCodec,
    VideoEncoding,
};
use crate::{
    AudioFollow, BusId, BusInfo, BusMonitor, MAX_BUSES, MAX_MONITOR_DEVICE_LEN, MonitorTap,
    SendInfo, SourceAudio,
};
use crate::{MixInput, MixResolution, VideoMix, VideoMixId, VideoMixInfo};

/// The revision of a collection that started empty at rev 0 once [`collection`] has been
/// applied to it: the batch is applied as one batch, so exactly one revision later.
pub const COLLECTION_REV: Rev = Rev(1);

/// The kinds an implementation must describe for [`collection`] to apply: the source
/// kind `webcam`, described as the engine's built-in kind is, whose one parameter
/// `device` the collection leaves at its default; `desktop_audio` and `mic`, audio kinds
/// with no video and no parameters; and the audio filter kind `gain`, whose one
/// parameter the collection always sets, so its settings read back as given.
pub fn kinds() -> Vec<KindDescriptor> {
    vec![
        KindDescriptor {
            kind_id: "webcam".into(),
            category: KindCategory::Source,
            name: "Webcam".into(),
            version: 1,
            video: true,
            audio: true,
            max_outset_px: None,
            params: vec![ParamDescriptor {
                id: "device".into(),
                ty: ParamType::String {
                    multiline: false,
                    max_len: 1024,
                },
                default: serde_json::Value::from(""),
                label: "Device".into(),
                description: Some(
                    "The ID of the webcam or capture card, as the platform lists it. Empty \
                     captures the first device listed."
                        .into(),
                ),
                group: None,
                advanced: false,
                visible_if: None,
                live: false,
            }],
        },
        KindDescriptor {
            kind_id: "display".into(),
            category: KindCategory::Source,
            name: "Display capture".into(),
            version: 1,
            video: true,
            audio: false,
            max_outset_px: None,
            params: vec![
                ParamDescriptor {
                    id: "target".into(),
                    ty: ParamType::CaptureTarget {
                        kind: crate::CaptureTargetKind::Display,
                        max_len: 1024,
                    },
                    default: serde_json::Value::from(""),
                    label: "Display".into(),
                    description: Some(
                        "The ID of the display to capture, as `capture.targets` lists it. Empty \
                     captures the primary display."
                            .into(),
                    ),
                    group: None,
                    advanced: false,
                    visible_if: None,
                    live: false,
                },
                ParamDescriptor {
                    id: "show_cursor".into(),
                    ty: ParamType::Bool,
                    default: serde_json::Value::from(true),
                    label: "Show cursor".into(),
                    description: Some("Whether the pointer shows in the capture.".into()),
                    group: None,
                    advanced: false,
                    visible_if: None,
                    live: false,
                },
            ],
        },
        KindDescriptor {
            kind_id: "desktop_audio".into(),
            category: KindCategory::Source,
            name: "Desktop audio".into(),
            version: 1,
            video: false,
            audio: true,
            max_outset_px: None,
            params: vec![],
        },
        KindDescriptor {
            kind_id: "mic".into(),
            category: KindCategory::Source,
            name: "Microphone".into(),
            version: 1,
            video: false,
            audio: true,
            max_outset_px: None,
            params: vec![],
        },
        KindDescriptor {
            kind_id: "gain".into(),
            category: KindCategory::AudioFilter,
            name: "Gain".into(),
            version: 1,
            video: false,
            audio: true,
            max_outset_px: None,
            params: vec![ParamDescriptor {
                id: "gain_db".into(),
                ty: ParamType::Float {
                    min: Some(-96.0),
                    max: Some(24.0),
                    step: None,
                    unit: Some("dB".into()),
                },
                default: serde_json::Value::from(0.0),
                label: "Gain".into(),
                description: None,
                group: None,
                advanced: false,
                visible_if: None,
                live: true,
            }],
        },
    ]
}

/// The batch that builds the fixture collection from an empty one: a 1280×720 canvas,
/// scenes `b` then `a` (created `a` first, then reordered), source `cam` placed twice in
/// `a` (the second placement moved, cropped, hidden, set to opacity 0.25 and reordered to
/// the bottom) and once in `b` with a geometry and opacity 0.5, scene `b` nested in `a` through the scene source `b_src`, the
/// RTMP outputs `yt` and `tw` (`tw`'s target changed after it was created), the
/// virtual camera `vcam`, the
/// recording output `rec`, the encoder profiles `hq` (updated after it was created,
/// used by `rec`) and `low` (used by `yt`, `tw` keeping the engine's default), and the
/// audio: the `mic` source and its copy `mic_copy`, the bus `vod` beside the master
/// (`drums` was created with a send and deleted again), sends and faders changed after
/// they were made, and `b_src`'s audio turned off. Then the
/// references and clones of ADR-0023: `cam` is duplicated as `cam_copy` and `i1` is
/// switched to place the copy, keeping its ID and position; `aux` is created with a
/// `duplicated_from` naming a source that doesn't exist, which reads as absent. Then the
/// video mixes: `side` shows scene `b` and is shown by `yt`, and `prog` (updated after
/// it was created) shows the program and is shown by `rec`; `tw` shows none. Last, the
/// hotkeys of ADR-0024 §5: `F1` switches to scene `a`, and `Ctrl+Digit1`, bound first to
/// `output.stop` and rebound by a clear and a set, starts output `yt`.
///
/// Sources, outputs, encoder profiles and video mixes are created in the opposite order
/// of their IDs, so reads that order them by ID can't pass by echoing creation order.
pub fn collection() -> Batch {
    let source = |s: SourceInfo| Edit::SourceCreate {
        source_id: s.source_id,
        name: s.name,
        kind: s.kind,
        settings: s.settings,
        duplicated_from: s.duplicated_from,
    };
    let gain = |id: &str, db: f32, index: Option<u32>| Edit::FilterAdd {
        owner: desk_owner(),
        chain: FilterChain::Audio,
        filter_id: FilterId::new(id),
        kind: "gain".into(),
        settings: gain_db(db),
        enabled: None,
        index,
    };
    let add = |item: &str, source: &str| Edit::ItemAdd {
        scene_id: SceneId::new("a"),
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
        transform: None,
        geometry: None,
        opacity: None,
        blend: None,
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
            source(screen()),
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
            Edit::ItemSetOpacity {
                item_id: ItemId::new("i3"),
                scene_id: None,
                opacity: QUARTER,
            },
            Edit::ItemSetBlend {
                item_id: ItemId::new("i3"),
                blend: BlendMode::Normal,
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
                opacity: Some(HALF),
                blend: Some(BlendMode::Normal),
            },
            Edit::OutputCreate {
                output_id: OutputId::new("yt"),
                kind: youtube().kind,
            },
            Edit::OutputCreate {
                output_id: OutputId::new("vcam"),
                kind: OutputKind::VirtualCam,
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
            Edit::VideoMixCreate {
                mix_id: VideoMixId::new("side"),
                name: side().name,
                mix: side().mix,
            },
            Edit::VideoMixCreate {
                mix_id: VideoMixId::new("prog"),
                name: "Draft".into(),
                mix: VideoMix {
                    input: MixInput::Preview,
                    resolution: MixResolution {
                        width: 1920,
                        height: 1080,
                    },
                    fps_divisor: 1,
                },
            },
            Edit::VideoMixUpdate {
                mix_id: VideoMixId::new("prog"),
                name: prog().name,
                mix: prog().mix,
            },
            Edit::OutputSetVideoMix {
                output_id: OutputId::new("rec"),
                mix_id: Some(VideoMixId::new("prog")),
            },
            Edit::OutputSetVideoMix {
                output_id: OutputId::new("yt"),
                mix_id: Some(VideoMixId::new("side")),
            },
            Edit::OutputSetVideoMix {
                output_id: OutputId::new("tw"),
                mix_id: Some(VideoMixId::new("side")),
            },
            Edit::OutputSetVideoMix {
                output_id: OutputId::new("tw"),
                mix_id: None,
            },
            source(desk()),
            gain("g1", -6.0, None),
            gain("g2", 0.0, Some(0)),
            Edit::FilterSetEnabled {
                owner: desk_owner(),
                filter_id: FilterId::new("g2"),
                enabled: false,
            },
            Edit::FilterSetSettings {
                owner: desk_owner(),
                filter_id: FilterId::new("g1"),
                patch: gain_db(-3.0),
            },
            Edit::FilterReorder {
                owner: desk_owner(),
                filter_id: FilterId::new("g1"),
                index: 0,
            },
            gain_on(FilterOwner::Master, "m1", FilterChain::Audio, -1.0),
            gain_on(rec_owner(), "o1", FilterChain::Audio, -6.0),
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
            Edit::BusSetMonitor {
                bus_id: BusId::new("drums"),
                monitor: Some(BusMonitor::default()),
            },
            // An output carries a bus (0.59), and moves off it before it is deleted.
            Edit::OutputSetAudioBuses {
                output_id: OutputId::new("rec"),
                audio_buses: vec![BusId::new("drums")],
            },
            Edit::OutputSetAudioBuses {
                output_id: OutputId::new("rec"),
                audio_buses: vec![BusId::new("vod")],
            },
            Edit::BusDelete {
                bus_id: BusId::new("drums"),
            },
            // A monitor set, changed and taken away (0.42); the master's is gone again.
            Edit::BusSetMonitor {
                bus_id: BusId::master(),
                monitor: Some(BusMonitor::default()),
            },
            Edit::BusSetMonitor {
                bus_id: BusId::new("vod"),
                monitor: Some(BusMonitor {
                    device: "headphones".into(),
                    tap: MonitorTap::PostFilters,
                }),
            },
            Edit::BusSetMonitor {
                bus_id: BusId::new("vod"),
                monitor: Some(BusMonitor {
                    device: "headphones".into(),
                    tap: MonitorTap::PreFilters,
                }),
            },
            Edit::BusSetMonitor {
                bus_id: BusId::master(),
                monitor: None,
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
            Edit::HotkeySet {
                chord: Chord::new("F1"),
                action: switch_to_a(),
            },
            Edit::HotkeySet {
                chord: Chord::new("Ctrl+Digit1"),
                action: Action::OutputStop {
                    output_id: OutputId::new("yt"),
                },
            },
            Edit::HotkeyClear {
                chord: Chord::new("Ctrl+Digit1"),
            },
            Edit::HotkeySet {
                chord: Chord::new("Ctrl+Digit1"),
                action: start_yt(),
            },
        ],
        expected_rev: Some(Rev(0)),
        idempotency_key: None,
        ends_gesture: None,
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
                        folder: None,
                        meta: Default::default(),
                    },
                    SceneSummary {
                        scene_id: scene("a"),
                        name: "A".into(),
                        role: SceneRole::Scene,
                        item_count: 3,
                        folder: None,
                        meta: Default::default(),
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
                        overrides: Vec::new(),
                        filters: Default::default(),
                        variant_of: None,
                        scene_id: scene("b"),
                        name: "B".into(),
                        role: SceneRole::Group,
                        transition_in: None,
                        transition_out: None,
                        items: items_of_b(),
                        folder: None,
                        meta: Default::default(),
                        options: Default::default(),
                    },
                    SceneInfo {
                        overrides: Vec::new(),
                        filters: Default::default(),
                        variant_of: None,
                        scene_id: scene("a"),
                        name: "A".into(),
                        role: SceneRole::Scene,
                        transition_in: None,
                        transition_out: None,
                        items: items_of_a(),
                        folder: None,
                        meta: Default::default(),
                        options: Default::default(),
                    },
                ],
                outputs: outputs(),
                master: Box::new(MasterInfo {
                    filters: audio_chain(vec![FilterInfo {
                        filter_id: FilterId::new("m1"),
                        kind: "gain".into(),
                        enabled: true,
                        settings: gain_db(-1.0),
                    }]),
                }),
                encoders: encoders(),
                transitions: Box::new(TransitionsInfo::default()),
                video_mixes: video_mixes(),
                folders: Vec::new(),
                labels: Vec::new(),
                buses: buses(),
                sends: sends(),
                hotkeys: hotkeys(),
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
        // And video mixes.
        (
            Query::VideoMixList,
            Ok(QueryResult::VideoMixList {
                rev: COLLECTION_REV,
                video_mixes: video_mixes(),
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
                ends_gesture: None,
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
                ends_gesture: None,
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
                opacity: None,
                blend: None,
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
                opacity: None,
                blend: None,
            }),
            invalid(
                "edits.0.geometry",
                "send either `transform` or `geometry`, not both",
            ),
        ),
        (
            Batch::single(Edit::ItemSetOpacity {
                item_id: ItemId::new("i1"),
                scene_id: None,
                opacity: finite(1.5),
            }),
            invalid("edits.0.opacity", "must be from 0 to 1"),
        ),
        (
            Batch::single(Edit::ItemAdd {
                scene_id: SceneId::new("a"),
                item_id: ItemId::new("i9"),
                source_id: SourceId::new("cam"),
                transform: None,
                geometry: None,
                opacity: Some(finite(-0.5)),
                blend: None,
            }),
            invalid("edits.0.opacity", "must be from 0 to 1"),
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
            // A tilted item so wide that its near-plane clip lands past MAX_DRAWN_PX.
            Batch::single(Edit::ItemSetGeometry {
                item_id: ItemId::new("i1"),
                scene_id: None,
                geometry: ItemGeometry {
                    size: Size {
                        width: finite(1e13),
                        height: finite(2000.0),
                    },
                    tilt_deg: finite(60.0),
                    perspective_px: Some(finite(800.0)),
                    ..BOXED
                },
            }),
            invalid("edits.0.geometry", "the item draws too far from the canvas"),
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
                ends_gesture: None,
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
                kind: record("\\\\host\\share\\show.mp4"),
            }),
            invalid("edits.0.kind.path", "must not start with two separators"),
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: record("/home/me/Videos/"),
            }),
            invalid("edits.0.kind.path", "must end in a file name"),
        ),
        (
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("new"),
                kind: record("C:\\Videos\\nul.mp4"),
            }),
            invalid("edits.0.kind.path", "must not end in a DOS device name"),
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
        (
            Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("vcam"),
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
                ends_gesture: None,
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
        // Filters (ADR-0026): the owner must exist, IDs are unique within it, a scene
        // source has no audio chain, and indexes are bounded by the chain.
        (
            Batch::single(filter_add("ghost", "g", FilterChain::Audio, None)),
            ProtocolError::NotFound {
                entity: EntityKind::Source,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(filter_add("desk", "", FilterChain::Audio, None)),
            invalid("edits.0.filter_id", "must not be empty"),
        ),
        (
            Batch::single(filter_add("desk", "g1", FilterChain::Audio, None)),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Filter,
                id: "g1".into(),
            },
        ),
        (
            Batch::single(filter_add("b_src", "g", FilterChain::Audio, None)),
            invalid("edits.0.chain", "source `b_src` has no audio chain"),
        ),
        (
            Batch::single(filter_add("desk", "g3", FilterChain::Audio, Some(3))),
            invalid(
                "edits.0.index",
                "must be less than the number of positions in the chain (3)",
            ),
        ),
        (
            Batch::single(Edit::FilterRemove {
                owner: desk_owner(),
                filter_id: FilterId::new("ghost"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Filter,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::FilterReorder {
                owner: desk_owner(),
                filter_id: FilterId::new("g2"),
                index: 2,
            }),
            invalid(
                "edits.0.index",
                "must be less than the number of filters in the chain (2)",
            ),
        ),
        // The chain holds at most eight: six more fit beside `g1` and `g2`, a ninth not.
        (
            Batch {
                edits: (3..=9)
                    .map(|i| filter_add("desk", &format!("g{i}"), FilterChain::Audio, None))
                    .collect(),
                expected_rev: None,
                idempotency_key: None,
                ends_gesture: None,
            },
            invalid("edits.6.chain", "a chain holds at most 8 filters"),
        ),
        // The master stage and outputs (ADR-0080): audio chains only, on an output
        // that exists.
        (
            Batch::single(gain_on(FilterOwner::Master, "v", FilterChain::Video, 0.0)),
            invalid(
                "edits.0.chain",
                "the master stage's and outputs' video chains are not available yet; only \
                 their audio chains hold filters",
            ),
        ),
        // A scene's filters and an item's effects (0.43, ADR-0065): a video chain only,
        // on a scene or item that exists.
        (
            Batch::single(gain_on(
                FilterOwner::Scene {
                    scene_id: SceneId::new("a"),
                },
                "s",
                FilterChain::Audio,
                0.0,
            )),
            invalid(
                "edits.0.chain",
                "scene `a` has no audio chain: a scene's audio is its bus's",
            ),
        ),
        (
            Batch::single(gain_on(
                FilterOwner::Item {
                    item_id: ItemId::new("i1"),
                },
                "e",
                FilterChain::Audio,
                0.0,
            )),
            invalid(
                "edits.0.chain",
                "item `i1` has no audio chain: its effects are video only",
            ),
        ),
        (
            Batch::single(Edit::FilterRemove {
                owner: FilterOwner::Item {
                    item_id: ItemId::new("ghost"),
                },
                filter_id: FilterId::new("e"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Item,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::FilterRemove {
                owner: FilterOwner::Scene {
                    scene_id: SceneId::new("ghost"),
                },
                filter_id: FilterId::new("s"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(gain_on(
                FilterOwner::Output {
                    output_id: OutputId::new("ghost"),
                },
                "g",
                FilterChain::Audio,
                0.0,
            )),
            ProtocolError::NotFound {
                entity: EntityKind::Output,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(gain_on(rec_owner(), "o1", FilterChain::Audio, 0.0)),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Filter,
                id: "o1".into(),
            },
        ),
        (
            Batch::single(gain_on(
                FilterOwner::Output {
                    output_id: OutputId::new("vcam"),
                },
                "g",
                FilterChain::Audio,
                0.0,
            )),
            invalid(
                "edits.0.chain",
                "output `vcam` carries no audio, so it has no audio chain",
            ),
        ),
        // A reference is refused before the index is checked.
        (
            Batch::single(Edit::FilterAdd {
                owner: FilterOwner::Master,
                chain: FilterChain::Audio,
                filter_id: FilterId::new("k"),
                kind: "gain".into(),
                settings: [(
                    "key".to_owned(),
                    serde_json::json!({"type": "source", "source_id": "desk"}),
                )]
                .into_iter()
                .collect(),
                enabled: None,
                index: Some(5),
            }),
            invalid(
                "edits.0.settings.key",
                "a master or output filter can't hold a reference yet",
            ),
        ),
        (
            Batch::single(Edit::VideoMixCreate {
                mix_id: VideoMixId::new("side"),
                name: "Again".into(),
                mix: side().mix,
            }),
            ProtocolError::AlreadyExists {
                entity: EntityKind::VideoMix,
                id: "side".into(),
            },
        ),
        (
            Batch::single(Edit::VideoMixCreate {
                mix_id: VideoMixId::new(""),
                name: "Empty".into(),
                mix: side().mix,
            }),
            invalid("edits.0.mix_id", "must not be empty"),
        ),
        (
            Batch::single(Edit::VideoMixUpdate {
                mix_id: VideoMixId::new("ghost"),
                name: "Ghost".into(),
                mix: side().mix,
            }),
            ProtocolError::NotFound {
                entity: EntityKind::VideoMix,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::VideoMixCreate {
                mix_id: VideoMixId::new("new"),
                name: "Odd".into(),
                mix: VideoMix {
                    resolution: MixResolution {
                        width: 1279,
                        height: 720,
                    },
                    ..prog().mix
                },
            }),
            invalid(
                "edits.0.mix.resolution.width",
                "must be an even number from 16 to 8192",
            ),
        ),
        (
            Batch::single(Edit::VideoMixUpdate {
                mix_id: VideoMixId::new("prog"),
                name: "Still".into(),
                mix: VideoMix {
                    fps_divisor: 0,
                    ..prog().mix
                },
            }),
            invalid("edits.0.mix.fps_divisor", "must be from 1 to 60"),
        ),
        (
            Batch::single(Edit::VideoMixCreate {
                mix_id: VideoMixId::new("new"),
                name: "Later".into(),
                mix: VideoMix {
                    input: MixInput::Unknown,
                    ..prog().mix
                },
            }),
            invalid(
                "edits.0.mix.input.type",
                "is not a mix input this engine knows",
            ),
        ),
        (
            Batch::single(Edit::VideoMixCreate {
                mix_id: VideoMixId::new("new"),
                name: "Ghost scene".into(),
                mix: VideoMix {
                    input: MixInput::Scene {
                        scene_id: SceneId::new("ghost"),
                    },
                    ..prog().mix
                },
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::OutputSetVideoMix {
                output_id: OutputId::new("tw"),
                mix_id: Some(VideoMixId::new("ghost")),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::VideoMix,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::OutputSetVideoMix {
                output_id: OutputId::new("ghost"),
                mix_id: None,
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Output,
                id: "ghost".into(),
            },
        ),
        // A mix an output shows can't be deleted, until the output moves off it.
        (
            Batch::single(Edit::VideoMixDelete {
                mix_id: VideoMixId::new("prog"),
            }),
            invalid(
                "edits.0.mix_id",
                "video mix `prog` is used by output `rec`; point it elsewhere first",
            ),
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
                ends_gesture: None,
            },
            invalid(
                "edits.14.bus_id",
                &format!("a collection holds at most {MAX_BUSES} buses, the master included"),
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
        // An output's audio buses (0.59): one bus that exists, on an output with audio.
        (
            Batch::single(Edit::OutputSetAudioBuses {
                output_id: OutputId::new("tw"),
                audio_buses: vec![BusId::new("ghost")],
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Bus,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::OutputSetAudioBuses {
                output_id: OutputId::new("ghost"),
                audio_buses: vec![BusId::master()],
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Output,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::OutputSetAudioBuses {
                output_id: OutputId::new("tw"),
                audio_buses: vec![],
            }),
            invalid(
                "edits.0.audio_buses",
                "an output carries exactly one bus: it writes one audio track",
            ),
        ),
        (
            Batch::single(Edit::OutputSetAudioBuses {
                output_id: OutputId::new("tw"),
                audio_buses: vec![BusId::master(), BusId::new("vod")],
            }),
            invalid(
                "edits.0.audio_buses",
                "an output carries exactly one bus: it writes one audio track",
            ),
        ),
        (
            Batch::single(Edit::OutputSetAudioBuses {
                output_id: OutputId::new("vcam"),
                audio_buses: vec![BusId::new("vod")],
            }),
            invalid(
                "edits.0.audio_buses",
                "output `vcam` has no audio: a virtual camera sends video only",
            ),
        ),
        // A bus an output carries can't be deleted: the output blocks it.
        (
            Batch::single(Edit::BusDelete {
                bus_id: BusId::new("vod"),
            }),
            ProtocolError::InvalidArgument {
                field: "edits.0.bus_id".into(),
                reason: "bus `vod` is still referred to by output `rec` (audio_buses); remove \
                         those references first, or send the batch `delete.plan` returns"
                    .into(),
                blockers: vec![crate::Blocker {
                    entity: EntityKind::Output,
                    id: "rec".into(),
                    field: "audio_buses".into(),
                }],
            },
        ),
        // Monitors (0.42): the bus must exist, and `BusMonitor::problem` decides the rest.
        (
            Batch::single(Edit::BusSetMonitor {
                bus_id: BusId::new("ghost"),
                monitor: None,
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Bus,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::BusSetMonitor {
                bus_id: BusId::master(),
                monitor: Some(BusMonitor {
                    device: String::new(),
                    tap: MonitorTap::Unknown,
                }),
            }),
            invalid(
                "edits.0.monitor.tap",
                "is not a tap point this engine knows",
            ),
        ),
        (
            Batch::single(Edit::BusSetMonitor {
                bus_id: BusId::new("vod"),
                monitor: Some(BusMonitor {
                    device: "d".repeat(MAX_MONITOR_DEVICE_LEN + 1),
                    tap: MonitorTap::PreFilters,
                }),
            }),
            invalid(
                "edits.0.monitor.device",
                &format!("must be at most {MAX_MONITOR_DEVICE_LEN} bytes"),
            ),
        ),
        // Hotkeys (ADR-0024 §5): one spelling per chord, one binding per chord, and only
        // actions that name objects that exist and keep no secret.
        (
            Batch::single(Edit::HotkeySet {
                chord: Chord::new("Shift+Ctrl+KeyA"),
                action: start_yt(),
            }),
            invalid(
                "edits.0.chord",
                "modifiers must each appear once, in the order Ctrl, Alt, Shift, Meta",
            ),
        ),
        (
            Batch::single(Edit::HotkeySet {
                chord: Chord::new("F1"),
                action: start_yt(),
            }),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Hotkey,
                id: "F1".into(),
            },
        ),
        (
            Batch::single(Edit::HotkeySet {
                chord: Chord::new("F2"),
                action: Action::SceneSwitch {
                    scene_id: SceneId::new("ghost"),
                    preset_id: None,
                },
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "ghost".into(),
            },
        ),
        (
            Batch::single(Edit::HotkeySet {
                chord: Chord::new("F2"),
                action: Action::OutputSetKey {
                    output_id: OutputId::new("yt"),
                    key: crate::SecretString::new("live_secret"),
                },
            }),
            invalid(
                "edits.0.action.type",
                "`output.set_key` can't be bound: a stream key is never stored in the \
                 collection (R-SEC-001)",
            ),
        ),
        (
            Batch::single(Edit::HotkeyClear {
                chord: Chord::new("F2"),
            }),
            ProtocolError::NotFound {
                entity: EntityKind::Hotkey,
                id: "F2".into(),
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
                source_id: SourceId::new("screen"),
                enabled: Some(false),
                follow: None,
                gain_db: None,
                muted: None,
            }),
            invalid("edits.0.source_id", "source `screen` has no audio"),
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
                source_id: SourceId::new("screen"),
                bus_id: BusId::new("vod"),
                gain_db: Finite32::ZERO,
            }),
            invalid("edits.0.source_id", "source `screen` has no audio"),
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
            monitor: None,
        },
        BusInfo {
            bus_id: BusId::new("vod"),
            name: "VOD track".into(),
            gain_db: finite(-6.0),
            muted: true,
            monitor: Some(BusMonitor {
                device: "headphones".into(),
                tap: MonitorTap::PreFilters,
            }),
        },
    ]
}

/// The collection's sends, ordered by bus, then by source: the copy keeps `mic`'s send
/// into `vod`, and lost its master send; `desk` keeps its default master send.
fn sends() -> Vec<SendInfo> {
    let send = |source: &str, bus: BusId, gain_db: f32| SendInfo {
        source_id: SourceId::new(source),
        bus_id: bus,
        gain_db: finite(gain_db),
    };
    vec![
        send("aux", BusId::master(), 0.0),
        send("cam", BusId::master(), 0.0),
        send("cam_copy", BusId::master(), 0.0),
        send("desk", BusId::master(), 0.0),
        send("mic", BusId::master(), -1.5),
        send("mic", BusId::new("vod"), -3.0),
        send("mic_copy", BusId::new("vod"), -3.0),
    ]
}

/// A `gain` filter at `db` on `owner`'s `chain`, last.
fn gain_on(owner: FilterOwner, id: &str, chain: FilterChain, db: f32) -> Edit {
    Edit::FilterAdd {
        owner,
        chain,
        filter_id: FilterId::new(id),
        kind: "gain".into(),
        settings: gain_db(db),
        enabled: None,
        index: None,
    }
}

/// The output `rec`, as a filter owner.
fn rec_owner() -> FilterOwner {
    FilterOwner::Output {
        output_id: OutputId::new("rec"),
    }
}

/// Chains with `audio` and no video filter.
fn audio_chain(audio: Vec<FilterInfo>) -> FilterChains {
    FilterChains {
        video: vec![],
        audio,
    }
}

/// Mix `side`: scene `b` at 640×360 and half the base rate.
fn side() -> VideoMixInfo {
    VideoMixInfo {
        mix_id: VideoMixId::new("side"),
        name: "Side".into(),
        mix: VideoMix {
            input: MixInput::Scene {
                scene_id: SceneId::new("b"),
            },
            resolution: MixResolution {
                width: 640,
                height: 360,
            },
            fps_divisor: 2,
        },
    }
}

/// Mix `prog`, as `video_mix.update` left it: the program at 1280×720 and the base rate.
fn prog() -> VideoMixInfo {
    VideoMixInfo {
        mix_id: VideoMixId::new("prog"),
        name: "Program 720p".into(),
        mix: VideoMix {
            input: MixInput::Program,
            resolution: MixResolution {
                width: 1280,
                height: 720,
            },
            fps_divisor: 1,
        },
    }
}

/// The collection's video mixes, ordered by ID.
fn video_mixes() -> Vec<VideoMixInfo> {
    vec![prog(), side()]
}

/// A `gain` filter on `source`, at 0 dB.
fn filter_add(source: &str, id: &str, chain: FilterChain, index: Option<u32>) -> Edit {
    Edit::FilterAdd {
        owner: FilterOwner::Source {
            source_id: SourceId::new(source),
        },
        chain,
        filter_id: FilterId::new(id),
        kind: "gain".into(),
        settings: gain_db(0.0),
        enabled: None,
        index,
    }
}

/// The action `F1` runs.
fn switch_to_a() -> Action {
    Action::SceneSwitch {
        scene_id: SceneId::new("a"),
        preset_id: None,
    }
}

/// The action `Ctrl+Digit1` runs.
fn start_yt() -> Action {
    Action::OutputStart {
        output_id: OutputId::new("yt"),
    }
}

/// The collection's hotkey bindings, ordered by chord.
pub fn hotkeys() -> Vec<HotkeyBinding> {
    vec![
        HotkeyBinding::new(Chord::new("Ctrl+Digit1"), &start_yt()),
        HotkeyBinding::new(Chord::new("F1"), &switch_to_a()),
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
        filters: audio_chain(vec![FilterInfo {
            filter_id: FilterId::new("o1"),
            kind: "gain".into(),
            enabled: true,
            settings: gain_db(-6.0),
        }]),
        video_mix: Some(VideoMixId::new("prog")),
        audio_buses: vec![BusId::new("vod")],
    }
}

fn youtube() -> OutputInfo {
    OutputInfo {
        output_id: OutputId::new("yt"),
        kind: rtmp("rtmps://a.rtmps.youtube.com:443/live2"),
        encoder_profile: Some(EncoderProfileId::new("low")),
        filters: FilterChains::default(),
        video_mix: Some(VideoMixId::new("side")),
        audio_buses: vec![BusId::master()],
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
            filters: FilterChains::default(),
            video_mix: None,
            audio_buses: vec![BusId::master()],
        },
        OutputInfo {
            output_id: OutputId::new("vcam"),
            kind: OutputKind::VirtualCam,
            encoder_profile: None,
            filters: FilterChains::default(),
            video_mix: None,
            audio_buses: vec![BusId::master()],
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
            filters: FilterChains {
                video: vec![],
                audio: vec![
                    FilterInfo {
                        filter_id: FilterId::new("g1"),
                        kind: "gain".into(),
                        enabled: true,
                        settings: gain_db(-3.0),
                    },
                    FilterInfo {
                        filter_id: FilterId::new("g2"),
                        kind: "gain".into(),
                        enabled: false,
                        settings: gain_db(0.0),
                    },
                ],
            },
            ..desk()
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
        screen(),
    ]
}

/// Desktop audio source `desk`, before its filters.
fn desk() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("desk"),
        name: "Desktop".into(),
        kind: SourceKind::Builtin {
            kind: "desktop_audio".into(),
        },
        settings: crate::Settings::new(),
        audio: Some(SourceAudio::new(AudioFollow::Always)),
        ..cam()
    }
}

fn desk_owner() -> FilterOwner {
    FilterOwner::Source {
        source_id: SourceId::new("desk"),
    }
}

/// A `gain` filter's settings.
fn gain_db(db: f32) -> crate::Settings {
    let mut settings = crate::Settings::new();
    settings.insert("gain_db", serde_json::Value::from(f64::from(db)));
    settings
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
        filters: Default::default(),
        meta: Default::default(),
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

/// A webcam as `source.create` makes it: a kind with video and audio (its paired audio)
/// follows `when_shown`.
fn cam() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("cam"),
        name: "Camera".into(),
        kind: SourceKind::Builtin {
            kind: "webcam".into(),
        },
        settings: [("device".to_owned(), serde_json::Value::from(""))]
            .into_iter()
            .collect(),
        duplicated_from: None,
        filters: Default::default(),
        meta: Default::default(),
        audio: Some(SourceAudio::new(AudioFollow::WhenShown)),
    }
}

/// A display capture, never placed: the collection's one source with no audio.
fn screen() -> SourceInfo {
    SourceInfo {
        source_id: SourceId::new("screen"),
        name: "Screen".into(),
        kind: SourceKind::Builtin {
            kind: "display".into(),
        },
        settings: [
            ("target".to_owned(), serde_json::Value::from("")),
            ("show_cursor".to_owned(), serde_json::Value::from(true)),
        ]
        .into_iter()
        .collect(),
        audio: None,
        ..cam()
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
        filters: Default::default(),
        meta: Default::default(),
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

/// Item `i4`'s geometry: a 320×180 box centred at (640, 360), filled and mirrored left
/// to right.
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
    flip_h: true,
    flip_v: false,
};

/// `ItemInfo.transform` for [`BOXED`]: its unrotated top-left corner at scale (1, 1).
const BOXED_VIEW: ItemTransform = ItemTransform {
    position: Vec2 {
        x: finite(480.0),
        y: finite(270.0),
    },
    ..ItemTransform::IDENTITY
};

/// `i4`'s opacity, set by `item.add`.
const HALF: Finite32 = match Finite32::new(0.5) {
    Some(value) => value,
    None => Finite32::ONE,
};

/// `i3`'s opacity, set by `item.set_opacity`.
const QUARTER: Finite32 = match Finite32::new(0.25) {
    Some(value) => value,
    None => Finite32::ONE,
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
        effects: Vec::new(),
        opacity: Finite32::ONE,
        blend: BlendMode::Normal,
        transition: ItemTransition::default(),
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
            effects: Vec::new(),
            opacity: QUARTER,
            blend: BlendMode::Normal,
            transition: ItemTransition::default(),
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
        effects: Vec::new(),
        opacity: HALF,
        blend: BlendMode::Normal,
        transition: ItemTransition::default(),
    }]
}
