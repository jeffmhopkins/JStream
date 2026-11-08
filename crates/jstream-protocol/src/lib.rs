//! The JStream control API: the messages that the engine, the web UI, the CLI and MCP
//! agents all speak. Commands are edits (undoable, batched, revision-checked) or actions
//! (side effects); queries read; events push changes; errors carry stable codes. The JSON
//! Schema is generated from these types. Layer 1: plain data, WASM-safe, no I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-protocol/README.md#purpose, crates/jstream-protocol/README.md#contents-commands, crates/jstream-protocol/README.md#contents-state, crates/jstream-protocol/README.md#contents-calls, crates/jstream-protocol/README.md#contents-errors, crates/jstream-protocol/README.md#contents-geometry, crates/jstream-protocol/README.md#versions, crates/jstream-protocol/README.md#contents-schema, crates/jstream-protocol/README.md#design, crates/jstream-protocol/README.md#calls, crates/jstream-protocol/README.md#placement, crates/jstream-protocol/README.md#audio, crates/jstream-protocol/README.md#kinds, crates/jstream-protocol/README.md#filters, crates/jstream-protocol/README.md#outputs, crates/jstream-protocol/README.md#encoder-profiles, crates/jstream-protocol/README.md#hotkeys, crates/jstream-protocol/README.md#bindings, crates/jstream-protocol/README.md#organisation, crates/jstream-protocol/README.md#video-mixes, crates/jstream-protocol/README.md#transitions, crates/jstream-protocol/README.md#batches, crates/jstream-protocol/README.md#queries, crates/jstream-protocol/README.md#undo, crates/jstream-protocol/README.md#live-values, crates/jstream-protocol/README.md#events, crates/jstream-protocol/README.md#youtube, crates/jstream-protocol/README.md#plugins, crates/jstream-protocol/README.md#quarantine, crates/jstream-protocol/README.md#errors, crates/jstream-protocol/README.md#compatibility, crates/jstream-protocol/README.md#geometry, crates/jstream-protocol/README.md#binary-streams, docs/adr/0013-command-model.md, docs/adr/0022-item-transform-and-canvas.md, docs/adr/0025-typed-settings-kind-descriptors.md, docs/adr/0058-youtube-sign-in-with-a-jstream-client.md, crates/jstream-protocol/README.md#shared-values, crates/jstream-protocol/README.md#motion, crates/jstream-protocol/README.md#codec-downloads, crates/jstream-protocol/README.md#timelines, docs/adr/0115-timelines-on-the-wire.md

#![forbid(unsafe_code)]

mod activity;
mod audio;
mod binding;
mod camera_motion;
mod clap;
mod command;
mod encoder;
mod envelope;
mod error;
mod event;
mod filter;
mod finite;
mod frames;
mod geometry;
mod history;
mod hotkey;
mod id;
mod keyframe;
mod kind;
mod live;
mod mix;
mod motion;
mod organise;
mod output;
mod pack;
#[cfg(feature = "fixtures")]
pub mod parity;
mod plugin;
mod preview;
mod profile;
mod query;
mod schema;
mod settings;
mod timeline;
mod transition;
mod values;
mod version;
mod youtube;

pub use activity::{ActivityEntry, MAX_ACTIVITY_ENTRIES, MAX_ACTIVITY_SUMMARY_CHARS};
pub use audio::{
    AudioFollow, BusInfo, BusMeter, BusMonitor, Loudness, MAX_BUSES, MAX_GAIN_DB,
    MAX_MONITOR_DEVICE_LEN, MIN_GAIN_DB, MonitorTap, SendInfo, SourceAudio, SourceMeter,
    gain_db_problem,
};
pub use binding::{Binding, MAX_INPUT_NAME_LEN, Trigger};
pub use camera_motion::{
    CameraFollow, CameraMotionModifier, CameraProperty, FOLLOW_POINTER_KIND, FOLLOW_ZOOM_RANGE,
    FOLLOW_ZOOM_STEP, MAX_FOLLOW_MARGIN_PCT, check_camera_motion, follow_pointer_descriptor,
};
pub use clap::{ClapFailed, ClapFolder, ClapFound, ClapScanInfo};
pub use command::{
    Action, Batch, BlendMode, Blending, Command, CommandClass, Crop, Edit, EffectsSpace,
    ItemTransform, MAX_LEGACY_SCALE, MAX_OPACITY, SceneRole, SourceKind, Vec2,
};
pub use encoder::{
    AudioCodec, AudioEncoding, CodecDownloadInfo, EncoderProfile, EncoderProfileInfo,
    EncoderSessionInfo, HardwarePreference, RateControl, VideoCodec, VideoEncoding,
};
pub use envelope::{
    ActionResult, Call, CallResult, ClientMessage, ErrorResponse, EventMessage, Hello,
    NetworkAccess, Request, Response, ServerMessage, Welcome,
};
pub use error::{Blocker, EntityKind, ProtocolError, WireError};
pub use event::{
    ChainCost, CollectionCause, DropCounts, EngineLoad, Event, KindsCause, OutputHealth,
    OutputState, OutputStatsInfo,
};
pub use filter::{FilterChain, FilterChains, FilterInfo, FilterOwner, MAX_CHAIN_FILTERS};
pub use finite::Finite32;
pub use frames::{
    FrameSheet, FrameStrip, MAX_SHEET_FRAMES, MAX_SHEET_SPAN_MS, MIN_SHEET_INTERVAL_MS, SheetFrame,
};
pub use geometry::{
    CameraView, Canvas, CanvasPoint, ContentRect, DEFAULT_FOV_DEG, DrawnPolygon, DrawnVertex,
    FitMode, Focus, GeometryError, ItemGeometry, LegacyBox, LegacyQuad, LocalPoint, MAX_BLUR_PX,
    MAX_DRAWN_PX, MAX_FOV_DEG, MIN_CAMERA_DISTANCE, MIN_FOV_DEG, NEAR_W, Placed, PlacementForm,
    ProjectedQuad, SceneCamera, Size, Solid, SolidBounds, Vec3, legacy_quad,
};
pub use history::{HistoryCall, HistoryDirection, HistoryOp};
pub use hotkey::{Chord, HotkeyBinding};
pub use id::{
    BusId, ClientId, DeviceId, EncoderProfileId, FilterId, FolderId, GestureId, IdempotencyKey,
    ItemId, LabelId, OutputId, PluginId, RequestId, Rev, SceneId, SourceId, TimelineId,
    TransitionPresetId, VideoMixId,
};
pub use keyframe::{DroppedOverride, ItemOverride, OverrideField, PlacedKeyframe};
pub use kind::{
    CAMERA_CONTROL_AUTO, CaptureFormatPart, EnumOption, KindCategory, KindDescriptor, KindSummary,
    MAX_CAMERA_CONTROL_NAME, MAX_CAMERA_CONTROLS, MAX_CAPTURE_FPS, MAX_CAPTURE_SIDE, Media,
    PIXEL_FORMATS, ParamDescriptor, ParamType, VisibleIf, check_asset_path, not_a_reference,
    parse_resolution,
};
pub use live::{
    GestureInfo, LiveOutcome, MAX_GESTURES_PER_CLIENT, MAX_LIVE_VALUES, is_live_adjustable,
};
pub use mix::{
    DEFAULT_BASE_FPS, FAST_BASE_FPS, MixInput, MixResolution, VideoMix, VideoMixInfo,
    allowed_base_fps,
};
pub use motion::{
    MAX_MOTION_ID_LEN, MAX_MOTION_MODIFIERS, MotionClock, MotionCurve, MotionKind, MotionMeasure,
    MotionModifier, MotionProblem, MotionProperty, MotionShape, check_motion, check_motion_id,
};
pub use organise::{
    FolderInfo, LabelInfo, LabelMembers, LabelledScene, LabelledTimeline, MAX_ICON_LEN, SceneMeta,
    SourceMeta, check_color, check_icon, parse_rgba,
};
pub use output::{
    OutputInfo, OutputKind, RecordContainer, RtmpTarget, SecretString,
    rtmp_url_sends_key_in_cleartext,
};
pub use pack::{
    MAX_PACK_FILES, MAX_PACK_IMAGE_BYTES, MAX_PACK_WRITE_BYTES, PackContent, PackFile,
    PackFileInfo, PackInfo,
};
pub use plugin::{
    PLUGIN_SCOPES, PluginClass, PluginDenial, PluginDisabledReason, PluginGrant, PluginInfo,
    PluginRequest, PluginScope, PluginState, Reach, plugin_classes, plugin_may_bind, plugin_scope,
    settings_reach,
};
pub use preview::{
    BINARY_HEADER_LEN, BINARY_VERSION, BinaryError, BinaryFrame, BinaryHeader, KeyframeGate,
    MAX_SESSION_VIEWS, MAX_VIEW_LABEL_CHARS, PreviewCodec, PreviewDraft, PreviewSettings,
    StreamKind, ViewCounts, check_view_label,
};
pub use profile::{
    DEFAULT_PROFILE_SECONDS, DEFAULT_PROFILE_TOP, Drawer, DrawerProfile, MAX_PROFILE_SECONDS,
    MAX_PROFILE_TOP, MixerProfile, PhaseProfile, PlanProfile, ProfileInfo, ProfileStream,
    ProfileUnit, SourceProfile, StreamProfile, ThreadProfile, UnitProfile, ViewProfile,
};
pub use query::{
    CameraControlInfo, CaptureFormatInfo, CaptureTargetInfo, CaptureTargetKind, DeleteTarget,
    DeviceInfo, FontInfo, ItemInfo, MasterInfo, PixelSize, Query, QueryResult, SceneInfo,
    SceneOptions, SceneSummary, SourceInfo, SourceSize,
};
pub use schema::json_schema;
pub use settings::{AssetRoot, Reference, Settings};
pub use timeline::{
    MAX_CUES, MAX_TIMELINE_ID_LEN, MAX_TIMELINE_MS, MAX_TIMELINE_NAME_LEN, MAX_TIMELINES,
    MIN_LOOP_MS, PlayingTimeline, TimelineChange, TimelineCue, TimelineInfo, TimelineLoop,
    check_timeline_id,
};
pub use transition::{
    Bezier, CUT, Easing, FADE, Glide, INITIAL_PRESET, ItemEasing, ItemTransition, KeyframeTiming,
    MAX_BEZIER_OVERSHOOT, MAX_DELAY_MS, MAX_DURATION_MS, MAX_STAGGER_MS, MOVE, STINGER,
    TransitionEffect, TransitionPresetInfo, TransitionStarted, TransitionsInfo,
    builtin_kinds as builtin_transition_kinds, check_preset_timing, initial_preset,
};
pub use values::{
    MAX_VALUE_BYTES, MAX_VALUE_NAME_BYTES, MAX_VALUES, SharedValue, check_value_name,
    check_value_size, prefix_covers,
};
pub use version::{PROTOCOL_VERSION, ProtocolVersion};
pub use youtube::{
    AudienceStatsInfo, BroadcastInfo, BroadcastLifeCycle, BroadcastListStatus, BroadcastPrivacy,
    BroadcastTransition, BroadcastType, HealthIssue, IssueSeverity, QuotaInfo, StatsPolling,
    StreamHealth, StreamInfo, StreamStatus, YoutubeAuth,
};

#[cfg(test)]
mod fixtures;

// The two tests below sit at the crate root because RULES.md names them by path:
// `jstream_protocol::names_are_valid` (R-API-002) and
// `jstream_protocol::every_command_is_classified` (R-ARCH-006).

/// R-API-002: every command, query and event is named `noun.verb` in snake_case, names
/// are unique, and the derived MCP tool names are valid.
#[cfg(test)]
#[test]
fn names_are_valid() {
    fixtures::check_names_are_valid();
}

/// ADR-0069: every command has a place in the plugin capability table, in a class or
/// under "never", so no new command is granted to plugins without a PR deciding it.
#[cfg(test)]
#[test]
fn every_command_has_a_plugin_scope() {
    fixtures::check_every_command_has_a_plugin_scope();
}

/// R-ARCH-006: every command in the schema is classified as exactly one of edit or
/// action, and every edit is batchable and undoable while no action is.
#[cfg(test)]
#[test]
fn every_command_is_classified() {
    fixtures::check_every_command_is_classified();
}
