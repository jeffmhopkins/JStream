//! Test fixtures: one value of every message, command, query, event and error variant,
//! and the checks the crate-root tests run over them.
//!
//! The fixtures are checked against the generated schema, so a variant added without a
//! fixture fails `fixtures_cover_every_variant_in_the_schema`, and through it the
//! naming and classification checks see every variant.

use crate::{
    Action, Batch, BlendMode, Blocker, Call, CallResult, Canvas, ClientId, ClientMessage, Command,
    CommandClass, Crop, DeleteTarget, DropCounts, Edit, EntityKind, ErrorResponse, Event,
    EventMessage, FilterChain, FilterChains, FilterId, FilterInfo, FilterOwner, Finite32, FitMode,
    Hello, IdempotencyKey, ItemGeometry, ItemId, ItemInfo, ItemTransform, MasterInfo, OutputId,
    OutputInfo, OutputKind, OutputState, PROTOCOL_VERSION, PlacementForm, ProtocolError, Query,
    QueryResult, RecordContainer, Request, RequestId, Response, Rev, RtmpTarget, SceneId,
    SceneInfo, SceneRole, SceneSummary, SecretString, ServerMessage, Settings, Size, SourceId,
    SourceInfo, SourceKind, Vec2, Welcome, json_schema,
};
use crate::{
    ActionResult, AudioCodec, AudioEncoding, BroadcastInfo, BroadcastLifeCycle,
    BroadcastListStatus, BroadcastPrivacy, BroadcastTransition, BroadcastType, EncoderProfile,
    EncoderProfileId, EncoderProfileInfo, EncoderSessionInfo, HardwarePreference, HealthIssue,
    IssueSeverity, RateControl, StreamHealth, StreamInfo, StreamStatus, VideoCodec, VideoEncoding,
    YoutubeAuth,
};
use crate::{AudienceStatsInfo, QuotaInfo, StatsPolling};
use crate::{AudioFollow, BusId, BusInfo, BusMeter, Loudness, SendInfo, SourceAudio, SourceMeter};
use crate::{CaptureTargetInfo, CaptureTargetKind, PixelSize};
use crate::{Chord, DeviceId, DeviceInfo, HotkeyBinding};
use crate::{DroppedOverride, ItemOverride, OverrideField};
use crate::{
    Easing, Glide, ItemEasing, ItemTransition, TransitionEffect, TransitionPresetId,
    TransitionPresetInfo, TransitionStarted, TransitionsInfo,
};
use crate::{
    EnumOption, KindCategory, KindDescriptor, KindSummary, Media, ParamDescriptor, ParamType,
    VisibleIf,
};
use crate::{FolderId, FolderInfo, LabelId, LabelInfo, SceneMeta, SourceMeta};
use crate::{GestureId, GestureInfo, LiveOutcome};
use crate::{HistoryCall, HistoryDirection, HistoryOp, PreviewCodec};
use crate::{MixInput, MixResolution, VideoMix, VideoMixId, VideoMixInfo};
use crate::{PluginClass, PluginDisabledReason, PluginGrant, PluginId, PluginState};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) fn edits() -> Vec<Edit> {
    let mut edits = vec![
        Edit::SceneCreate {
            scene_id: SceneId::new("scn_1"),
            name: "Main".into(),
            role: None,
        },
        Edit::SceneCreate {
            scene_id: SceneId::new("scn_grp"),
            name: "Lower third".into(),
            role: Some(SceneRole::Group),
        },
        Edit::SceneRename {
            scene_id: SceneId::new("scn_1"),
            name: "Intro".into(),
        },
        Edit::SceneDelete {
            scene_id: SceneId::new("scn_1"),
        },
        Edit::SceneSetRole {
            scene_id: SceneId::new("scn_grp"),
            role: SceneRole::Group,
        },
        Edit::ItemMove {
            item_id: ItemId::new("itm_2"),
            scene_id: SceneId::new("scn_grp"),
            index: 0,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("scn_1"),
            item_id: ItemId::new("itm_1"),
            source_id: SourceId::new("src_cam"),
            transform: None,
            geometry: None,
            opacity: None,
            blend: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("scn_1"),
            item_id: ItemId::new("itm_2"),
            source_id: SourceId::new("src_cam"),
            transform: Some(ItemTransform::IDENTITY),
            geometry: None,
            opacity: None,
            blend: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("scn_1"),
            item_id: ItemId::new("itm_3"),
            source_id: SourceId::new("src_cam"),
            transform: None,
            geometry: Some(corner_box()),
            opacity: Finite32::new(0.75),
            blend: Some(BlendMode::Normal),
        },
        Edit::ItemSetOpacity {
            item_id: ItemId::new("itm_3"),
            scene_id: None,
            opacity: Finite32::new(0.5).expect("finite"),
        },
        Edit::ItemRemove {
            item_id: ItemId::new("itm_1"),
        },
        Edit::ItemSetTransform {
            item_id: ItemId::new("itm_2"),
            scene_id: None,
            transform: ItemTransform {
                position: Vec2::new(1280.0, 0.5).expect("finite"),
                scale: Vec2::new(0.25, f32::MAX).expect("finite"),
                rotation_deg: Finite32::new(-90.0).expect("finite"),
            },
        },
        Edit::ItemSetGeometry {
            item_id: ItemId::new("itm_2"),
            scene_id: None,
            geometry: ItemGeometry {
                fit_mode: FitMode::Fill,
                rotation_deg: Finite32::new(12.5).expect("finite"),
                flip_h: true,
                ..ItemGeometry::covering_canvas(1920, 1080).expect("positive")
            },
        },
        Edit::CanvasSetSize {
            width: 3840,
            height: 2160,
        },
        Edit::ItemSetVisible {
            item_id: ItemId::new("itm_2"),
            scene_id: None,
            visible: false,
        },
        Edit::ItemSetVisible {
            item_id: ItemId::new("itm_2"),
            scene_id: Some(SceneId::new("scn_1")),
            visible: true,
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
            scene_id: None,
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
            duplicated_from: None,
        },
        Edit::SourceCreate {
            source_id: SourceId::new("src_bg"),
            name: "Background".into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: color_settings(),
            duplicated_from: None,
        },
        Edit::SourceCreate {
            source_id: SourceId::new("src_intro"),
            name: "Intro scene".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("scn_1"),
            },
            settings: Settings::new(),
            duplicated_from: None,
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
        Edit::SourceCreate {
            source_id: SourceId::new("src_cam_2"),
            name: "Camera 2".into(),
            kind: SourceKind::Builtin {
                kind: "webcam".into(),
            },
            settings: Settings::new(),
            duplicated_from: Some(SourceId::new("src_cam")),
        },
        Edit::ItemSetSource {
            item_id: ItemId::new("itm_2"),
            source_id: SourceId::new("src_cam_2"),
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
        Edit::OutputCreate {
            output_id: OutputId::new("out_rec"),
            kind: OutputKind::Record {
                path: "/home/me/Videos/show.mp4".into(),
                container: RecordContainer::Mp4,
            },
        },
        Edit::OutputCreate {
            output_id: OutputId::new("out_live"),
            kind: OutputKind::Rtmp {
                target: RtmpTarget::YouTube {
                    broadcast_id: "dQw4w9WgXcQ".into(),
                },
            },
        },
        Edit::OutputCreate {
            output_id: OutputId::new("out_cam"),
            kind: OutputKind::VirtualCam,
        },
        Edit::EncoderCreate {
            profile_id: EncoderProfileId::new("enc_rec"),
            name: "Recording".into(),
            profile: recording_profile(),
        },
        Edit::EncoderUpdate {
            profile_id: EncoderProfileId::new("enc_rec"),
            name: "Recording (hardware)".into(),
            profile: EncoderProfile {
                video: VideoEncoding {
                    hardware: HardwarePreference::Require,
                    ..EncoderProfile::DEFAULT.video
                },
                ..EncoderProfile::DEFAULT
            },
        },
        Edit::OutputSetEncoder {
            output_id: OutputId::new("out_rec"),
            profile_id: Some(EncoderProfileId::new("enc_rec")),
        },
        Edit::EncoderDelete {
            profile_id: EncoderProfileId::new("enc_old"),
        },
        Edit::TransitionCreate {
            preset_id: TransitionPresetId::new("tr_move"),
            name: "Smooth move".into(),
            kind: "move".into(),
            duration_ms: 600,
            easing: Easing::EaseInOut,
            settings: [("unmatched", json!("slide"))].into_iter().collect(),
        },
        Edit::TransitionUpdate {
            preset_id: TransitionPresetId::new("tr_move"),
            name: Some("Springy move".into()),
            kind: None,
            duration_ms: Some(800),
            easing: Some(Easing::Spring),
            patch: [("stagger_ms", json!(40))].into_iter().collect(),
        },
        Edit::TransitionUpdate {
            preset_id: TransitionPresetId::new("tr_fade"),
            name: None,
            kind: Some("fade".into()),
            duration_ms: None,
            easing: Some(Easing::EaseOut),
            patch: Settings::new(),
        },
        Edit::TransitionUpdate {
            preset_id: TransitionPresetId::new("tr_fade"),
            name: None,
            kind: None,
            duration_ms: None,
            easing: Some(Easing::Linear),
            patch: Settings::new(),
        },
        Edit::TransitionSetDefault {
            preset_id: TransitionPresetId::new("tr_move"),
        },
        Edit::TransitionDelete {
            preset_id: TransitionPresetId::new("tr_old"),
        },
        Edit::SceneSetTransitions {
            scene_id: SceneId::new("scn_1"),
            transition_in: Some(TransitionPresetId::new("tr_fade")),
            transition_out: None,
        },
        Edit::ItemSetTransition {
            item_id: ItemId::new("itm_1"),
            transition: ItemTransition {
                delay_ms: 120,
                easing: ItemEasing::Spring,
                glide: Glide::Never,
                appear: TransitionEffect::Scale,
                disappear: TransitionEffect::None,
            },
        },
        Edit::ItemSetTransition {
            item_id: ItemId::new("itm_2"),
            transition: ItemTransition {
                easing: ItemEasing::EaseOut,
                appear: TransitionEffect::Slide,
                disappear: TransitionEffect::Fade,
                ..ItemTransition::default()
            },
        },
        Edit::ItemSetTransition {
            item_id: ItemId::new("itm_3"),
            transition: ItemTransition {
                easing: ItemEasing::Linear,
                ..ItemTransition::default()
            },
        },
        Edit::ItemSetTransition {
            item_id: ItemId::new("itm_4"),
            transition: ItemTransition {
                easing: ItemEasing::EaseInOut,
                ..ItemTransition::default()
            },
        },
        Edit::VideoMixCreate {
            mix_id: VideoMixId::new("mix_cam"),
            name: "Camera".into(),
            mix: camera_mix(),
        },
        Edit::VideoMixUpdate {
            mix_id: VideoMixId::new("mix_cam"),
            name: "Program 720p".into(),
            mix: program_mix(),
        },
        Edit::OutputSetVideoMix {
            output_id: OutputId::new("out_rec"),
            mix_id: Some(VideoMixId::new("mix_cam")),
        },
        Edit::OutputSetVideoMix {
            output_id: OutputId::new("out_yt"),
            mix_id: None,
        },
        Edit::VideoMixDelete {
            mix_id: VideoMixId::new("mix_old"),
        },
        Edit::FolderCreate {
            folder_id: FolderId::new("fld_shows"),
            name: "Shows".into(),
            parent: None,
        },
        Edit::FolderCreate {
            folder_id: FolderId::new("fld_intro"),
            name: "Intros".into(),
            parent: Some(FolderId::new("fld_shows")),
        },
        Edit::FolderRename {
            folder_id: FolderId::new("fld_intro"),
            name: "Openers".into(),
        },
        Edit::FolderSetParent {
            folder_id: FolderId::new("fld_intro"),
            parent: None,
        },
        Edit::FolderReorder {
            folder_id: FolderId::new("fld_intro"),
            index: 0,
        },
        Edit::FolderDelete {
            folder_id: FolderId::new("fld_old"),
        },
        Edit::SceneSetFolder {
            scene_id: SceneId::new("scn_1"),
            folder_id: Some(FolderId::new("fld_shows")),
        },
        Edit::SceneSetMeta {
            scene_id: SceneId::new("scn_1"),
            meta: scene_meta(),
        },
        Edit::LabelCreate {
            label_id: LabelId::new("lbl_sponsor"),
            name: "Sponsor".into(),
            color: "#ff8800".into(),
            icon: Some("megaphone".into()),
        },
        Edit::LabelUpdate {
            label_id: LabelId::new("lbl_sponsor"),
            name: "Sponsors".into(),
            color: "#ffaa00".into(),
            icon: None,
        },
        Edit::LabelReorder {
            label_id: LabelId::new("lbl_sponsor"),
            index: 0,
        },
        Edit::LabelDelete {
            label_id: LabelId::new("lbl_old"),
        },
        Edit::SourceSetMeta {
            source_id: SourceId::new("src_cam"),
            meta: SourceMeta {
                labels: vec![LabelId::new("lbl_sponsor")],
                favourite: true,
            },
        },
        Edit::SourceSetAudio {
            source_id: SourceId::new("src_mic"),
            enabled: Some(true),
            follow: Some(AudioFollow::Always),
            gain_db: Some(db(-6.0)),
            muted: Some(false),
        },
        Edit::SourceSetAudio {
            source_id: SourceId::new("src_intro"),
            enabled: None,
            follow: Some(AudioFollow::WhenShown),
            gain_db: None,
            muted: None,
        },
        Edit::BusCreate {
            bus_id: BusId::new("bus_vod"),
            name: "VOD".into(),
        },
        Edit::BusRename {
            bus_id: BusId::new("bus_vod"),
            name: "VOD track".into(),
        },
        Edit::BusSetFader {
            bus_id: BusId::master(),
            gain_db: Some(db(-3.0)),
            muted: Some(false),
        },
        Edit::SendSet {
            source_id: SourceId::new("src_mic"),
            bus_id: BusId::new("bus_vod"),
            gain_db: db(0.0),
        },
        Edit::SendRemove {
            source_id: SourceId::new("src_music"),
            bus_id: BusId::new("bus_vod"),
        },
        Edit::BusDelete {
            bus_id: BusId::new("bus_old"),
        },
        Edit::HotkeySet {
            chord: Chord::new("Ctrl+Digit1"),
            action: Action::SceneSwitch {
                scene_id: SceneId::new("scn_1"),
                preset_id: None,
            },
        },
        Edit::HotkeyClear {
            chord: Chord::new("Ctrl+Digit1"),
        },
    ];
    // Every blend mode, though the engine draws only `normal` so far.
    edits.extend(BlendMode::ALL.map(|blend| Edit::ItemSetBlend {
        item_id: ItemId::new("itm_3"),
        blend,
    }));
    edits.extend(filter_edits());
    edits.extend(keyframe_edits());
    edits
}

/// A keyframe scene's edits (protocol 0.39): creating one, writing an override through
/// a placement edit's `scene_id`, and clearing it.
fn keyframe_edits() -> Vec<Edit> {
    let mut edits = vec![
        Edit::SceneCreateVariant {
            scene_id: SceneId::new("scn_1_wide"),
            variant_of: SceneId::new("scn_1"),
            name: "Main, wide".into(),
        },
        Edit::ItemSetOpacity {
            item_id: ItemId::new("itm_2"),
            scene_id: Some(SceneId::new("scn_1_wide")),
            opacity: Finite32::new(0.5).expect("finite"),
        },
        Edit::ItemClearOverride {
            scene_id: SceneId::new("scn_1_wide"),
            item_id: ItemId::new("itm_2"),
            field: OverrideField::Opacity,
        },
    ];
    // Every field, as "reset keyframe" clears them.
    edits.extend(
        [
            OverrideField::Geometry,
            OverrideField::Crop,
            OverrideField::Visible,
        ]
        .map(|field| Edit::ItemClearOverride {
            scene_id: SceneId::new("scn_1_wide"),
            item_id: ItemId::new("itm_2"),
            field,
        }),
    );
    edits
}

/// What the keyframe scene `scn_1_wide` overrides.
fn item_overrides() -> Vec<ItemOverride> {
    vec![ItemOverride {
        transform: None,
        item_id: ItemId::new("itm_2"),
        geometry: Some(corner_box()),
        crop: None,
        visible: Some(false),
        opacity: Some(Finite32::new(0.5).expect("finite")),
    }]
}

/// The filter edits (ADR-0026), on both chains of a source, and on the master stage's
/// and an output's audio chains (ADR-0027).
fn filter_edits() -> Vec<Edit> {
    let mic = || FilterOwner::Source {
        source_id: SourceId::new("src_mic"),
    };
    let mut gain = Settings::new();
    gain.insert("gain_db", serde_json::json!(-6.0));
    vec![
        Edit::FilterAdd {
            owner: mic(),
            chain: FilterChain::Audio,
            filter_id: FilterId::new("flt_gain"),
            kind: "gain".into(),
            settings: gain.clone(),
            enabled: None,
            index: None,
        },
        Edit::FilterAdd {
            owner: FilterOwner::Source {
                source_id: SourceId::new("src_cam"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("flt_lut"),
            kind: "pack.lut".into(),
            settings: Settings::new(),
            enabled: Some(false),
            index: Some(0),
        },
        Edit::FilterSetEnabled {
            owner: mic(),
            filter_id: FilterId::new("flt_gain"),
            enabled: false,
        },
        Edit::FilterSetSettings {
            owner: mic(),
            filter_id: FilterId::new("flt_gain"),
            patch: gain,
        },
        Edit::FilterReorder {
            owner: mic(),
            filter_id: FilterId::new("flt_gain"),
            index: 0,
        },
        Edit::FilterRemove {
            owner: mic(),
            filter_id: FilterId::new("flt_gain"),
        },
        Edit::FilterAdd {
            owner: FilterOwner::Master,
            chain: FilterChain::Audio,
            filter_id: FilterId::new("flt_limit"),
            kind: "limiter".into(),
            settings: Settings::new(),
            enabled: None,
            index: None,
        },
        Edit::FilterAdd {
            owner: FilterOwner::Output {
                output_id: OutputId::new("out_rec"),
            },
            chain: FilterChain::Audio,
            filter_id: FilterId::new("flt_quiet"),
            kind: "gain".into(),
            settings: quieter(),
            enabled: None,
            index: None,
        },
    ]
}

/// A limiter's settings, complete as the engine reports them.
fn limiter() -> Settings {
    let mut settings = Settings::new();
    settings.insert("ceiling_db", serde_json::json!(-1.0));
    settings.insert("lookahead_ms", serde_json::json!(5.0));
    settings.insert("release_ms", serde_json::json!(60.0));
    settings
}

/// A gain filter's settings, 6 dB down.
fn quieter() -> Settings {
    let mut gain = Settings::new();
    gain.insert("gain_db", serde_json::json!(-6.0));
    gain
}

/// One scene at 1280×720 and half the base rate.
fn camera_mix() -> VideoMix {
    VideoMix {
        input: MixInput::Scene {
            scene_id: SceneId::new("scn_cam"),
        },
        resolution: MixResolution {
            width: 1280,
            height: 720,
        },
        fps_divisor: 2,
    }
}

/// The program at 1280×720 and the base rate.
fn program_mix() -> VideoMix {
    VideoMix {
        input: MixInput::Program,
        resolution: MixResolution {
            width: 1280,
            height: 720,
        },
        fps_divisor: 1,
    }
}

fn mix_infos() -> Vec<VideoMixInfo> {
    vec![
        VideoMixInfo {
            mix_id: VideoMixId::new("mix_cam"),
            name: "Camera".into(),
            mix: camera_mix(),
        },
        VideoMixInfo {
            mix_id: VideoMixId::new("mix_preview"),
            name: "Preview".into(),
            mix: VideoMix {
                input: MixInput::Preview,
                ..program_mix()
            },
        },
    ]
}

/// A finite gain in dB, or a finite level.
fn db(value: f32) -> Finite32 {
    Finite32::new(value).expect("finite")
}

fn bus_infos() -> Vec<BusInfo> {
    vec![
        BusInfo {
            bus_id: BusId::new("bus_vod"),
            name: "VOD track".into(),
            gain_db: db(0.0),
            muted: false,
        },
        BusInfo {
            bus_id: BusId::master(),
            name: "Master".into(),
            gain_db: db(-3.0),
            muted: false,
        },
    ]
}

fn send_infos() -> Vec<SendInfo> {
    vec![
        SendInfo {
            source_id: SourceId::new("src_mic"),
            bus_id: BusId::new("bus_vod"),
            gain_db: db(0.0),
        },
        SendInfo {
            source_id: SourceId::new("src_mic"),
            bus_id: BusId::master(),
            gain_db: db(-6.0),
        },
    ]
}

/// A scene's metadata with every field away from its default.
fn scene_meta() -> SceneMeta {
    SceneMeta {
        icon: Some("star".into()),
        color: Some("#1e1e2e".into()),
        favourite: true,
        show_in_multiview: false,
    }
}

/// The bindings the fixtures report: a scene switch and an output start.
fn hotkey_bindings() -> Vec<HotkeyBinding> {
    vec![
        HotkeyBinding::new(
            Chord::new("Ctrl+Digit1"),
            &Action::SceneSwitch {
                scene_id: SceneId::new("scn_1"),
                preset_id: None,
            },
        ),
        HotkeyBinding::new(
            Chord::new("F9"),
            &Action::OutputStart {
                output_id: OutputId::new("out_yt"),
            },
        ),
    ]
}

/// A recording profile: 12000 kbit/s VBR peaking at 20000, hardware preferred.
fn recording_profile() -> EncoderProfile {
    EncoderProfile {
        video: VideoEncoding {
            codec: VideoCodec::H264,
            rate_control: RateControl::Vbr {
                kbps: 12000,
                max_kbps: 20000,
            },
            keyframe_interval_s: 2,
            hardware: HardwarePreference::Prefer,
        },
        audio: AudioEncoding {
            codec: AudioCodec::Aac,
            kbps: 320,
        },
    }
}

fn encoder_infos() -> Vec<EncoderProfileInfo> {
    vec![
        EncoderProfileInfo {
            profile_id: EncoderProfileId::new("enc_rec"),
            name: "Recording".into(),
            profile: recording_profile(),
        },
        EncoderProfileInfo {
            profile_id: EncoderProfileId::new("enc_stream"),
            name: "Stream".into(),
            // The default on the software encoder, so a fixture uses `avoid`.
            profile: EncoderProfile {
                video: VideoEncoding {
                    hardware: HardwarePreference::Avoid,
                    ..EncoderProfile::DEFAULT.video
                },
                ..EncoderProfile::DEFAULT
            },
        },
        // HEVC and AV1 (0.41), for enhanced RTMP.
        EncoderProfileInfo {
            profile_id: EncoderProfileId::new("enc_hevc"),
            name: "HEVC stream".into(),
            profile: EncoderProfile {
                video: VideoEncoding {
                    codec: VideoCodec::Hevc,
                    hardware: HardwarePreference::Require,
                    ..EncoderProfile::DEFAULT.video
                },
                ..EncoderProfile::DEFAULT
            },
        },
        EncoderProfileInfo {
            profile_id: EncoderProfileId::new("enc_av1"),
            name: "AV1 stream".into(),
            profile: EncoderProfile {
                video: VideoEncoding {
                    codec: VideoCodec::Av1,
                    ..EncoderProfile::DEFAULT.video
                },
                ..EncoderProfile::DEFAULT
            },
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
    vec![
        OutputInfo {
            output_id: OutputId::new("out_rec"),
            kind: OutputKind::Record {
                path: "/home/me/Videos/show.mp4".into(),
                container: RecordContainer::Mp4,
            },
            encoder_profile: Some(EncoderProfileId::new("enc_rec")),
            filters: FilterChains {
                video: vec![],
                audio: vec![FilterInfo {
                    filter_id: FilterId::new("flt_quiet"),
                    kind: "gain".into(),
                    enabled: true,
                    settings: quieter(),
                }],
            },
            video_mix: Some(VideoMixId::new("mix_cam")),
        },
        OutputInfo {
            output_id: OutputId::new("out_yt"),
            kind: rtmp("rtmp://a.rtmp.youtube.com/live2"),
            encoder_profile: None,
            filters: FilterChains::default(),
            video_mix: None,
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
        Action::OutputSetKey {
            output_id: OutputId::new("out_yt"),
            key: SecretString::new("fixture-key-0000"),
        },
        Action::OutputClearKey {
            output_id: OutputId::new("out_yt"),
        },
        Action::LiveSet {
            gesture_id: GestureId::new("drag_1"),
            values: live_values(),
        },
        Action::LiveCancel {
            gesture_id: GestureId::new("drag_1"),
        },
        Action::YoutubeSignIn,
        Action::YoutubeSignOut,
        Action::YoutubeCreateBroadcast {
            title: "Friday stream".into(),
            description: "Building JStream live.".into(),
            scheduled_start_time: "2026-10-16T19:00:00Z".into(),
            privacy: BroadcastPrivacy::Unlisted,
            made_for_kids: false,
        },
        Action::YoutubeBindBroadcast {
            broadcast_id: "dQw4w9WgXcQ".into(),
            stream_id: Some("UCfixturechannelfixturestream1".into()),
        },
        Action::YoutubeBindBroadcast {
            broadcast_id: "dQw4w9WgXcQ".into(),
            stream_id: None,
        },
        Action::YoutubeTransitionBroadcast {
            broadcast_id: "dQw4w9WgXcQ".into(),
            to: BroadcastTransition::Testing,
        },
        Action::YoutubeTransitionBroadcast {
            broadcast_id: "dQw4w9WgXcQ".into(),
            to: BroadcastTransition::Live,
        },
        Action::YoutubeTransitionBroadcast {
            broadcast_id: "dQw4w9WgXcQ".into(),
            to: BroadcastTransition::Complete,
        },
        Action::AudioResetLoudness {},
        Action::PreviewSubscribe {
            channel: 1,
            max_width: Some(960),
            max_height: None,
            fps: Some(30),
            codecs: vec![PreviewCodec::H264, PreviewCodec::Jpeg],
        },
        Action::PreviewUnsubscribe { channel: 1 },
        Action::SceneSwitch {
            scene_id: SceneId::new("scn_2"),
            preset_id: None,
        },
        Action::SceneSwitch {
            scene_id: SceneId::new("scn_1"),
            preset_id: Some(TransitionPresetId::new("tr_move")),
        },
        Action::StudioSetPreview {
            scene_id: SceneId::new("scn_1"),
        },
        Action::PairingStart,
        Action::DeviceRevoke {
            device_id: DeviceId::new("dev_1"),
        },
        Action::PluginInstall {
            plugin_id: PluginId::new("plg_1"),
            path: "/home/me/plugins/chat-scenes".into(),
            grant: PluginGrant {
                classes: BTreeSet::from([PluginClass::Read, PluginClass::Edit, PluginClass::Live]),
                hosts: vec!["https://api.example.com:443".into()],
                folders: vec!["/home/me/clips".into()],
            },
        },
        Action::PluginGrant {
            plugin_id: PluginId::new("plg_1"),
            grant: PluginGrant {
                classes: BTreeSet::from([PluginClass::Outputs]),
                ..PluginGrant::default()
            },
        },
        Action::PluginEnable {
            plugin_id: PluginId::new("plg_1"),
        },
        Action::PrivacySet { on: true },
    ]
}

/// A two-item drag: one item moved, the other cropped.
fn live_values() -> Vec<Edit> {
    vec![
        Edit::ItemSetGeometry {
            item_id: ItemId::new("itm_1"),
            scene_id: None,
            geometry: corner_box(),
        },
        Edit::ItemSetCrop {
            item_id: ItemId::new("itm_2"),
            scene_id: None,
            crop: Crop {
                left: 4,
                top: 0,
                right: 4,
                bottom: 0,
            },
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
        Query::StudioGet,
        Query::PrivacyGet,
        Query::EncoderList,
        Query::VideoMixList,
        Query::CaptureTargets,
        Query::LiveList,
        Query::YoutubeStatus,
        Query::YoutubeListBroadcasts {
            status: BroadcastListStatus::All,
            broadcast_type: BroadcastType::All,
            page_token: None,
        },
        Query::YoutubeListBroadcasts {
            status: BroadcastListStatus::Upcoming,
            broadcast_type: BroadcastType::Event,
            page_token: Some("CAEQAA".into()),
        },
        Query::YoutubeListBroadcasts {
            status: BroadcastListStatus::Active,
            broadcast_type: BroadcastType::Persistent,
            page_token: None,
        },
        Query::YoutubeListBroadcasts {
            status: BroadcastListStatus::Completed,
            broadcast_type: BroadcastType::All,
            page_token: None,
        },
        Query::YoutubeListStreams {
            page_token: Some("CAEQAA".into()),
        },
        Query::BusList,
        Query::DeletePlan {
            target: DeleteTarget::Scene {
                scene_id: SceneId::new("scn_mask"),
            },
        },
        Query::DeletePlan {
            target: DeleteTarget::Source {
                source_id: SourceId::new("src_cam"),
            },
        },
        Query::DeletePlan {
            target: DeleteTarget::Folder {
                folder_id: FolderId::new("fld_shows"),
            },
        },
        Query::DeletePlan {
            target: DeleteTarget::Label {
                label_id: LabelId::new("lbl_sponsor"),
            },
        },
        Query::DeletePlan {
            target: DeleteTarget::Bus {
                bus_id: BusId::new("bus_vod"),
            },
        },
        Query::DeviceList,
        Query::YoutubeStats,
    ]
}

/// A live broadcast's audience, every count reported.
fn audience_stats() -> AudienceStatsInfo {
    AudienceStatsInfo {
        output_id: OutputId::new("out_live"),
        broadcast_id: "dQw4w9WgXcQ".into(),
        concurrent_viewers: Some(1520),
        likes: Some(3400),
        views: Some(98_765),
    }
}

/// A broadcast in every lifecycle state and privacy, the first with every field.
fn broadcasts() -> Vec<BroadcastInfo> {
    let lifecycles = [
        BroadcastLifeCycle::Created,
        BroadcastLifeCycle::Ready,
        BroadcastLifeCycle::TestStarting,
        BroadcastLifeCycle::Testing,
        BroadcastLifeCycle::LiveStarting,
        BroadcastLifeCycle::Live,
        BroadcastLifeCycle::Complete,
        BroadcastLifeCycle::Revoked,
        BroadcastLifeCycle::Other,
    ];
    let privacies = [
        BroadcastPrivacy::Unlisted,
        BroadcastPrivacy::Private,
        BroadcastPrivacy::Public,
        BroadcastPrivacy::Other,
    ];
    lifecycles
        .into_iter()
        .enumerate()
        .map(|(i, life_cycle)| BroadcastInfo {
            broadcast_id: format!("bc{i}"),
            title: "Friday stream".into(),
            description: if i == 0 {
                "Building JStream live.".into()
            } else {
                String::new()
            },
            scheduled_start_time: (i == 0).then(|| "2026-10-16T19:00:00Z".into()),
            life_cycle,
            privacy: privacies[i % privacies.len()],
            bound_stream_id: (i == 0).then(|| "UCfixturechannelfixturestream1".into()),
        })
        .collect()
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
            param("audio", ParamType::Bus, Value::Null),
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
            duplicated_from: None,
            filters: Default::default(),
            meta: Default::default(),
            audio: None,
        },
        SourceInfo {
            source_id: SourceId::new("src_cam"),
            name: "Camera".into(),
            kind: SourceKind::Builtin {
                kind: "webcam".into(),
            },
            settings: Settings::new(),
            duplicated_from: None,
            filters: Default::default(),
            meta: SourceMeta {
                labels: vec![LabelId::new("lbl_sponsor")],
                favourite: true,
            },
            audio: None,
        },
        SourceInfo {
            source_id: SourceId::new("src_cam_2"),
            name: "Camera 2".into(),
            kind: SourceKind::Builtin {
                kind: "webcam".into(),
            },
            settings: Settings::new(),
            duplicated_from: Some(SourceId::new("src_cam")),
            filters: FilterChains {
                video: vec![FilterInfo {
                    filter_id: FilterId::new("flt_lut"),
                    kind: "pack.lut".into(),
                    enabled: false,
                    settings: Settings::new(),
                }],
                audio: vec![],
            },
            meta: Default::default(),
            audio: Some(SourceAudio {
                enabled: false,
                ..SourceAudio::new(AudioFollow::WhenShown)
            }),
        },
        SourceInfo {
            source_id: SourceId::new("src_intro"),
            name: "Intro scene".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("scn_intro"),
            },
            settings: Settings::new(),
            duplicated_from: None,
            filters: Default::default(),
            meta: Default::default(),
            audio: Some(SourceAudio::new(AudioFollow::WhenShown)),
        },
        SourceInfo {
            source_id: SourceId::new("src_mic"),
            name: "Microphone".into(),
            kind: SourceKind::Builtin {
                kind: "audio_input".into(),
            },
            settings: Settings::new(),
            duplicated_from: None,
            audio: Some(SourceAudio {
                gain_db: db(-6.0),
                muted: true,
                ..SourceAudio::new(AudioFollow::Always)
            }),
            filters: Default::default(),
            meta: Default::default(),
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
                flip_v: true,
                ..corner_box()
            }),
            crop: Crop::NONE,
            visible: true,
            opacity: Finite32::new(0.5).expect("finite"),
            blend: BlendMode::Normal,
            transition: ItemTransition::default(),
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
            opacity: Finite32::ONE,
            blend: BlendMode::Normal,
            transition: ItemTransition {
                delay_ms: 80,
                glide: Glide::Never,
                ..ItemTransition::default()
            },
        },
    ]
}

fn transitions_info() -> TransitionsInfo {
    TransitionsInfo {
        presets: vec![
            crate::initial_preset(),
            TransitionPresetInfo {
                preset_id: TransitionPresetId::new("tr_move"),
                name: "Smooth move".into(),
                kind: "move".into(),
                duration_ms: 600,
                easing: Easing::EaseInOut,
                settings: [("stagger_ms", json!(0)), ("unmatched", json!("fade"))]
                    .into_iter()
                    .collect(),
            },
        ],
        default: TransitionPresetId::new("tr_move"),
    }
}

pub(crate) fn query_results() -> Vec<QueryResult> {
    vec![
        QueryResult::SceneList {
            rev: Rev(12),
            scenes: vec![SceneSummary {
                scene_id: SceneId::new("scn_1"),
                name: "Main".into(),
                role: SceneRole::Scene,
                item_count: 2,
                folder: None,
                meta: Default::default(),
            }],
        },
        QueryResult::OutputStatus {
            output_id: OutputId::new("out_yt"),
            state: OutputState::Active,
            key_set: true,
            encoder: Some(EncoderSessionInfo {
                profile_id: None,
                shared_with: vec![OutputId::new("out_tw")],
                hardware: false,
                dedicated: false,
            }),
            encoder_sessions: 2,
            hardware_sessions: 1,
            cleartext: false,
        },
        QueryResult::OutputStatus {
            output_id: OutputId::new("out_rec"),
            state: OutputState::Active,
            key_set: false,
            encoder: Some(EncoderSessionInfo {
                profile_id: None,
                shared_with: vec![],
                hardware: true,
                dedicated: true,
            }),
            encoder_sessions: 2,
            hardware_sessions: 1,
            cleartext: false,
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
                    overrides: Vec::new(),
                    variant_of: None,
                    scene_id: SceneId::new("scn_1"),
                    name: "Main".into(),
                    role: SceneRole::Scene,
                    transition_in: Some(TransitionPresetId::new("tr_move")),
                    transition_out: None,
                    items: item_infos(),
                    folder: Some(FolderId::new("fld_shows")),
                    meta: scene_meta(),
                },
                SceneInfo {
                    overrides: item_overrides(),
                    variant_of: Some(SceneId::new("scn_1")),
                    scene_id: SceneId::new("scn_1_wide"),
                    name: "Main, wide".into(),
                    role: SceneRole::Scene,
                    transition_in: None,
                    transition_out: None,
                    items: vec![],
                    folder: None,
                    meta: Default::default(),
                },
                SceneInfo {
                    overrides: Vec::new(),
                    variant_of: None,
                    scene_id: SceneId::new("scn_intro"),
                    name: "Intro".into(),
                    role: SceneRole::Group,
                    transition_in: None,
                    transition_out: Some(TransitionPresetId::new("cut")),
                    items: vec![],
                    folder: None,
                    meta: Default::default(),
                },
            ],
            outputs: output_infos(),
            encoders: encoder_infos(),
            transitions: Box::new(transitions_info()),
            video_mixes: mix_infos(),
            folders: vec![
                FolderInfo {
                    folder_id: FolderId::new("fld_shows"),
                    name: "Shows".into(),
                    parent: None,
                },
                FolderInfo {
                    folder_id: FolderId::new("fld_intro"),
                    name: "Intros".into(),
                    parent: Some(FolderId::new("fld_shows")),
                },
            ],
            labels: vec![LabelInfo {
                label_id: LabelId::new("lbl_sponsor"),
                name: "Sponsor".into(),
                color: "#ff8800".into(),
                icon: Some("megaphone".into()),
            }],
            buses: bus_infos(),
            sends: send_infos(),
            master: Box::new(MasterInfo {
                filters: FilterChains {
                    video: vec![],
                    audio: vec![FilterInfo {
                        filter_id: FilterId::new("flt_limit"),
                        kind: "limiter".into(),
                        enabled: true,
                        settings: limiter(),
                    }],
                },
            }),
            hotkeys: hotkey_bindings(),
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
                KindSummary {
                    kind_id: "move".into(),
                    category: KindCategory::Transition,
                    name: "Move".into(),
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
        QueryResult::StudioGet {
            program_scene: Some(SceneId::new("scn_2")),
            preview_scene: Some(SceneId::new("scn_1")),
        },
        QueryResult::StudioGet {
            program_scene: None,
            preview_scene: None,
        },
        QueryResult::PrivacyGet { on: true },
        QueryResult::EncoderList {
            rev: Rev(12),
            encoders: encoder_infos(),
        },
        QueryResult::VideoMixList {
            rev: Rev(12),
            video_mixes: mix_infos(),
        },
        QueryResult::CaptureTargets {
            targets: vec![
                CaptureTargetInfo {
                    kind: CaptureTargetKind::Display,
                    target: r"monitor:\\.\DISPLAY1".into(),
                    name: "DISPLAY1".into(),
                    size: Some(PixelSize {
                        width: 2560,
                        height: 1440,
                    }),
                    primary: true,
                },
                CaptureTargetInfo {
                    kind: CaptureTargetKind::Window,
                    target: "hwnd:0x1a2b".into(),
                    name: "Untitled - Notepad".into(),
                    size: None,
                    primary: false,
                },
            ],
        },
        QueryResult::LiveList {
            gestures: vec![GestureInfo {
                gesture_id: GestureId::new("drag_1"),
                client_id: ClientId::new("cli_7"),
                values: live_values(),
            }],
        },
        QueryResult::YoutubeStatus {
            auth: YoutubeAuth::SignedIn,
        },
        QueryResult::YoutubeListBroadcasts {
            broadcasts: broadcasts(),
            next_page_token: Some("CAEQAA".into()),
        },
        QueryResult::YoutubeStats {
            outputs: vec![audience_stats()],
            quota: QuotaInfo {
                used: 1234,
                daily: 10_000,
                exhausted: false,
                stats: StatsPolling::Normal,
            },
        },
        QueryResult::YoutubeStats {
            outputs: Vec::new(),
            quota: QuotaInfo {
                used: 10_000,
                daily: 10_000,
                exhausted: true,
                stats: StatsPolling::Stopped,
            },
        },
        QueryResult::YoutubeStats {
            outputs: Vec::new(),
            quota: QuotaInfo {
                used: 8500,
                daily: 10_000,
                exhausted: false,
                stats: StatsPolling::Slowed,
            },
        },
        QueryResult::YoutubeListStreams {
            streams: [
                (StreamStatus::Active, StreamHealth::Good),
                (StreamStatus::Created, StreamHealth::Ok),
                (StreamStatus::Error, StreamHealth::Bad),
                (StreamStatus::Inactive, StreamHealth::NoData),
                (StreamStatus::Ready, StreamHealth::Other),
                (StreamStatus::Other, StreamHealth::NoData),
            ]
            .into_iter()
            .enumerate()
            .map(|(i, (status, health))| StreamInfo {
                stream_id: format!("st{i}"),
                title: "Default stream key".into(),
                status,
                health,
            })
            .collect(),
            next_page_token: None,
        },
        QueryResult::BusList {
            rev: Rev(12),
            buses: bus_infos(),
            sends: send_infos(),
        },
        QueryResult::DeletePlan {
            rev: Rev(12),
            edits: vec![
                Edit::ItemRemove {
                    item_id: ItemId::new("itm_mask"),
                },
                Edit::SourceSetSettings {
                    source_id: SourceId::new("src_cam"),
                    patch: [("mask", Value::Null)].into_iter().collect(),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("src_mask_scene"),
                },
                Edit::SceneDelete {
                    scene_id: SceneId::new("scn_mask"),
                },
            ],
            hotkeys: vec![HotkeyBinding::new(
                Chord::new("F5"),
                &Action::SceneSwitch {
                    scene_id: SceneId::new("scn_mask"),
                    preset_id: None,
                },
            )],
        },
        QueryResult::DeviceList {
            devices: vec![DeviceInfo {
                device_id: DeviceId::new("dev_1"),
                name: "Tablet".into(),
                paired_at: 1_790_000_000,
                last_used_at: 1_790_003_600,
            }],
        },
    ]
}

pub(crate) fn events() -> Vec<Event> {
    let mut events = vec![
        Event::StateChanged {
            rev: Rev(13),
            client_id: ClientId::new("cli_7"),
            request_id: Some(RequestId::new("r1")),
            cause: None,
        },
        Event::StateChanged {
            rev: Rev(14),
            client_id: ClientId::new("cli_8"),
            request_id: None,
            cause: None,
        },
        Event::StateChanged {
            rev: Rev(15),
            client_id: ClientId::new("cli_7"),
            request_id: Some(RequestId::new("r30")),
            cause: Some(HistoryDirection::Undo),
        },
        Event::StateChanged {
            rev: Rev(16),
            client_id: ClientId::new("cli_7"),
            request_id: Some(RequestId::new("r31")),
            cause: Some(HistoryDirection::Redo),
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
    events.push(Event::StudioChanged {
        program_scene: Some(SceneId::new("scn_2")),
        preview_scene: Some(SceneId::new("scn_1")),
        transition: Some(TransitionStarted {
            preset_id: TransitionPresetId::new("tr_move"),
            duration_ms: 640,
        }),
        client_id: ClientId::new("cli_7"),
        request_id: Some(RequestId::new("r2")),
    });
    events.push(Event::StudioChanged {
        program_scene: None,
        preview_scene: None,
        transition: None,
        client_id: ClientId::new("cli_8"),
        request_id: None,
    });
    events.push(Event::PrivacyChanged {
        on: true,
        client_id: ClientId::new("cli_7"),
        request_id: Some(RequestId::new("r3")),
    });
    events.push(Event::EventsDropped { count: 1500 });
    events.push(Event::PluginStateChanged {
        plugin_id: PluginId::new("plg_1"),
        state: PluginState::Enabled,
        reason: None,
        remote_name: Some("Chat scenes".into()),
    });
    for reason in [
        PluginDisabledReason::CpuLimit,
        PluginDisabledReason::MemoryLimit,
        PluginDisabledReason::Trap,
        PluginDisabledReason::NeedsGrant,
    ] {
        events.push(Event::PluginStateChanged {
            plugin_id: PluginId::new("plg_1"),
            state: PluginState::Disabled,
            reason: Some(reason),
            remote_name: None,
        });
    }
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
    events.push(Event::LiveChanged {
        gesture_id: GestureId::new("drag_1"),
        client_id: ClientId::new("cli_7"),
        values: live_values(),
    });
    events.push(Event::LiveEnded {
        gesture_id: GestureId::new("drag_1"),
        client_id: ClientId::new("cli_7"),
        outcome: LiveOutcome::Committed,
        rev: Some(Rev(17)),
    });
    for outcome in [LiveOutcome::Cancelled, LiveOutcome::Expired] {
        events.push(Event::LiveEnded {
            gesture_id: GestureId::new("drag_2"),
            client_id: ClientId::new("cli_8"),
            outcome,
            rev: None,
        });
    }
    for auth in [
        YoutubeAuth::Unavailable,
        YoutubeAuth::SignedOut,
        YoutubeAuth::SigningIn,
        YoutubeAuth::SignedIn,
        YoutubeAuth::Other,
    ] {
        events.push(Event::YoutubeStatusChanged { auth });
    }
    events.push(Event::YoutubeHealthChanged {
        output_id: OutputId::new("out_live"),
        broadcast_id: "dQw4w9WgXcQ".into(),
        stream_id: "UCfixturechannelfixturestream1".into(),
        status: StreamStatus::Active,
        health: StreamHealth::Bad,
        issues: [
            IssueSeverity::Error,
            IssueSeverity::Warning,
            IssueSeverity::Info,
            IssueSeverity::Other,
        ]
        .into_iter()
        .map(|severity| HealthIssue {
            issue: "gopSizeLong".into(),
            severity,
            reason: "Check video settings".into(),
            description: "Please use a keyframe frequency of four seconds or less.".into(),
        })
        .collect(),
    });
    events.push(Event::YoutubeStats {
        stats: audience_stats(),
    });
    events.push(Event::YoutubeStats {
        stats: AudienceStatsInfo {
            output_id: OutputId::new("out_live"),
            broadcast_id: "dQw4w9WgXcQ".into(),
            concurrent_viewers: None,
            likes: None,
            views: None,
        },
    });
    events.push(Event::MetersTick {
        sources: vec![SourceMeter {
            source_id: SourceId::new("src_mic"),
            peak: [db(0.5), db(0.25)],
            rms: [db(0.125), db(0.0625)],
        }],
        buses: vec![
            BusMeter {
                bus_id: BusId::new("bus_vod"),
                peak: [db(0.5), db(0.25)],
                rms: [db(0.125), db(0.0625)],
                true_peak: None,
                loudness: None,
            },
            BusMeter {
                bus_id: BusId::master(),
                peak: [db(0.5), db(0.25)],
                rms: [db(0.125), db(0.0625)],
                true_peak: Some([db(0.53125), db(0.25)]),
                loudness: Some(Loudness {
                    momentary: Some(db(-18.5)),
                    short_term: Some(db(-20.0)),
                    integrated: None,
                }),
            },
        ],
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
        ProtocolError::invalid_argument("edits.0.name", "must not be empty"),
        ProtocolError::InvalidArgument {
            field: "edits.0.scene_id".into(),
            reason: "scene `scn_mask` is still referred to by source `src_mask_scene` \
                     (kind.scene_id), source `src_cam` (settings.mask); remove those \
                     references first, or send the batch `delete.plan` returns"
                .into(),
            blockers: vec![
                Blocker {
                    entity: EntityKind::Source,
                    id: "src_mask_scene".into(),
                    field: "kind.scene_id".into(),
                },
                Blocker {
                    entity: EntityKind::Source,
                    id: "src_cam".into(),
                    field: "settings.mask".into(),
                },
                Blocker {
                    entity: EntityKind::VideoMix,
                    id: "mix_mask".into(),
                    field: "mix.input.scene_id".into(),
                },
            ],
        },
        ProtocolError::UndoConflict {
            step_rev: Rev(9),
            client_id: ClientId::new("cli_8"),
            rev: Rev(11),
        },
        ProtocolError::HistoryEmpty {
            direction: HistoryDirection::Undo,
        },
        ProtocolError::HistoryEmpty {
            direction: HistoryDirection::Redo,
        },
        ProtocolError::ActionFailed {
            reason: "connection refused".into(),
        },
        ProtocolError::Unsupported {
            call: "pairing.start".into(),
        },
        ProtocolError::Forbidden {
            call: "output.start".into(),
            reason: "the plugin is not granted `outputs`".into(),
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
        EntityKind::Encoder,
        EntityKind::VideoMix,
        EntityKind::Filter,
        EntityKind::Bus,
        EntityKind::Device,
        EntityKind::Hotkey,
        EntityKind::Plugin,
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
            ends_gesture: None,
        }),
    ));
    msgs.push(request(
        2,
        Call::Batch(Batch {
            edits: live_values(),
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: Some(GestureId::new("drag_1")),
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
    msgs.push(request(
        30,
        Call::History(HistoryCall::new(HistoryOp::Undo)),
    ));
    msgs.push(request(
        31,
        Call::History(
            HistoryCall::new(HistoryOp::Redo)
                .expecting(Rev(15))
                .with_idempotency_key(IdempotencyKey::new("k-2")),
        ),
    ));
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
            result: CallResult::Batch {
                dropped_overrides: Vec::new(),
                rev: Rev(13),
                removed_hotkeys: vec![],
            },
        }),
        ServerMessage::Response(Response {
            id: RequestId::new("r1"),
            result: CallResult::Batch {
                dropped_overrides: item_overrides()
                    .into_iter()
                    .map(|dropped| DroppedOverride {
                        scene_id: SceneId::new("scn_1_wide"),
                        dropped,
                    })
                    .collect(),
                rev: Rev(14),
                removed_hotkeys: hotkey_bindings(),
            },
        }),
        ServerMessage::Response(Response {
            id: RequestId::new("r10"),
            result: CallResult::action(),
        }),
        ServerMessage::Response(Response {
            id: RequestId::new("r15"),
            result: CallResult::Action {
                result: Some(ActionResult::PairingStart {
                    code: SecretString::new("7K3M-9QXA"),
                    expires_in_secs: 120,
                }),
            },
        }),
        ServerMessage::Response(Response {
            id: RequestId::new("r30"),
            result: CallResult::History {
                rev: Rev(15),
                step_rev: Rev(13),
                undo_depth: 4,
                redo_depth: 1,
            },
        }),
        ServerMessage::Error(ErrorResponse {
            id: None,
            error: ProtocolError::HandshakeRequired.into(),
        }),
    ];
    let broadcast = broadcasts().swap_remove(0);
    for result in [
        ActionResult::YoutubeSignIn {
            url: "https://accounts.google.com/o/oauth2/v2/auth?client_id=1234-fixture.apps.googleusercontent.com&redirect_uri=http%3A%2F%2F127.0.0.1%3A53682&response_type=code&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fyoutube&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&state=fixture-state".into(),
        },
        ActionResult::YoutubeCreateBroadcast {
            broadcast: broadcast.clone(),
        },
        ActionResult::YoutubeBindBroadcast {
            broadcast: broadcast.clone(),
        },
        ActionResult::YoutubeTransitionBroadcast { broadcast },
    ] {
        msgs.push(ServerMessage::Response(Response {
            id: RequestId::new("r11"),
            result: CallResult::Action {
                result: Some(result),
            },
        }));
    }
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
        .chain(schema_tags(&schema, "HistoryOp", "type"))
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
            [HistoryOp::Undo, HistoryOp::Redo]
                .iter()
                .map(|h| (h.name(), serde_json::to_value(h))),
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

/// ADR-0069: every command a client can send (each edit, action, query and history op in
/// the schema) has a place in [`crate::PLUGIN_SCOPES`], and the table names nothing
/// else. A new command fails this until a PR places it in a class or under "never".
pub(crate) fn check_every_command_has_a_plugin_scope() {
    let schema = json_schema();
    let mut commands: BTreeSet<String> = ["Edit", "Action", "Query"]
        .into_iter()
        .flat_map(|def| schema_tags(&schema, def, "type"))
        .collect();
    commands.extend(
        [HistoryOp::Undo, HistoryOp::Redo]
            .into_iter()
            .map(|op| op.name().to_owned()),
    );
    let placed: BTreeSet<String> = crate::PLUGIN_SCOPES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    let unplaced: Vec<&String> = commands.difference(&placed).collect();
    assert!(unplaced.is_empty(), "no plugin scope for {unplaced:?}");
    let unknown: Vec<&String> = placed.difference(&commands).collect();
    assert!(
        unknown.is_empty(),
        "plugin scopes for unknown commands {unknown:?}"
    );
}

pub(crate) fn check_every_command_is_classified() {
    let schema = json_schema();
    let edit_names = schema_tags(&schema, "Edit", "type");
    let action_names = schema_tags(&schema, "Action", "type");
    assert!(
        edit_names.is_disjoint(&action_names),
        "a name is both an edit and an action"
    );
    // The only ways to send a command are a batch of edits or a single action. A
    // query reads, and a history call (ADR-0032) carries no command.
    let call_kinds = schema_tags(&schema, "Call", "kind");
    assert_eq!(
        call_kinds,
        BTreeSet::from([
            "action".into(),
            "batch".into(),
            "history".into(),
            "query".into()
        ])
    );
    check_history_call_carries_no_command(&schema, &edit_names, &action_names);

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

/// A `history` call's fields are its op, `expected_rev` and `idempotency_key`: no
/// `edits`, no `action`, and no op that names an edit or an action.
fn check_history_call_carries_no_command(
    schema: &Value,
    edit_names: &BTreeSet<String>,
    action_names: &BTreeSet<String>,
) {
    let history = schema["$defs"]["Call"]["oneOf"]
        .as_array()
        .expect("Call has oneOf")
        .iter()
        .find(|alt| alt["properties"]["kind"]["const"] == "history")
        .expect("Call has a history alternative");
    // The alternative tags `kind` and refers to `HistoryCall` for the rest.
    let call = history["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .expect("the history alternative refers to its struct");
    let fields: BTreeSet<&str> = [&history["properties"], &schema["$defs"][call]["properties"]]
        .into_iter()
        .filter_map(Value::as_object)
        .flat_map(|props| props.keys().map(String::as_str))
        .collect();
    assert_eq!(
        fields,
        BTreeSet::from(["kind", "op", "expected_rev", "idempotency_key"]),
        "a history call carries no command"
    );
    let ops = schema_tags(schema, "HistoryOp", "type");
    assert!(
        ops.is_disjoint(edit_names) && ops.is_disjoint(action_names),
        "a history op is not a command"
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
