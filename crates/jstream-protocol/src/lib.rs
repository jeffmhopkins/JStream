//! The JStream control API: the messages that the engine, the web UI, the CLI and MCP
//! agents all speak. Commands are edits (undoable, batched, revision-checked) or actions
//! (side effects); queries read; events push changes; errors carry stable codes. The JSON
//! Schema is generated from these types. Layer 1: plain data, WASM-safe, no I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-protocol/README.md#purpose, crates/jstream-protocol/README.md#design, docs/adr/0013-command-model.md, docs/adr/0022-item-transform-and-canvas.md, docs/adr/0025-typed-settings-kind-descriptors.md, docs/adr/0058-youtube-sign-in-with-a-jstream-client.md

#![forbid(unsafe_code)]

mod activity;
mod audio;
mod command;
mod encoder;
mod envelope;
mod error;
mod event;
mod filter;
mod finite;
mod geometry;
mod history;
mod hotkey;
mod id;
mod keyframe;
mod kind;
mod live;
mod mix;
mod organise;
mod output;
#[cfg(feature = "fixtures")]
pub mod parity;
mod plugin;
mod preview;
mod query;
mod schema;
mod settings;
mod transition;
mod version;
mod youtube;

pub use activity::{ActivityEntry, MAX_ACTIVITY_ENTRIES, MAX_ACTIVITY_SUMMARY_CHARS};
pub use audio::{
    AudioFollow, BusInfo, BusMeter, BusMonitor, Loudness, MAX_BUSES, MAX_GAIN_DB,
    MAX_MONITOR_DEVICE_LEN, MIN_GAIN_DB, MonitorTap, SendInfo, SourceAudio, SourceMeter,
    gain_db_problem,
};
pub use command::{
    Action, Batch, BlendMode, Command, CommandClass, Crop, Edit, ItemTransform, SceneRole,
    SourceKind, Vec2,
};
pub use encoder::{
    AudioCodec, AudioEncoding, EncoderProfile, EncoderProfileInfo, EncoderSessionInfo,
    HardwarePreference, RateControl, VideoCodec, VideoEncoding,
};
pub use envelope::{
    ActionResult, Call, CallResult, ClientMessage, ErrorResponse, EventMessage, Hello, Request,
    Response, ServerMessage, Welcome,
};
pub use error::{Blocker, EntityKind, ProtocolError, WireError};
pub use event::{DropCounts, EngineLoad, Event, OutputState};
pub use filter::{FilterChain, FilterChains, FilterInfo, FilterOwner, MAX_CHAIN_FILTERS};
pub use finite::Finite32;
pub use geometry::{
    Canvas, CanvasPoint, ContentRect, DrawnPolygon, DrawnVertex, FitMode, GeometryError,
    ItemGeometry, LegacyBox, LegacyQuad, LocalPoint, MAX_DRAWN_PX, NEAR_W, Placed, PlacementForm,
    ProjectedQuad, Size, legacy_quad,
};
pub use history::{HistoryCall, HistoryDirection, HistoryOp};
pub use hotkey::{Chord, HotkeyBinding};
pub use id::{
    BusId, ClientId, DeviceId, EncoderProfileId, FilterId, FolderId, GestureId, IdempotencyKey,
    ItemId, LabelId, OutputId, PluginId, RequestId, Rev, SceneId, SourceId, TransitionPresetId,
    VideoMixId,
};
pub use keyframe::{DroppedOverride, ItemOverride, OverrideField};
pub use kind::{
    EnumOption, KindCategory, KindDescriptor, KindSummary, Media, ParamDescriptor, ParamType,
    VisibleIf, check_asset_path,
};
pub use live::{
    GestureInfo, LiveOutcome, MAX_GESTURES_PER_CLIENT, MAX_LIVE_VALUES, is_live_adjustable,
};
pub use mix::{MixInput, MixResolution, VideoMix, VideoMixInfo};
pub use organise::{
    FolderInfo, LabelInfo, MAX_ICON_LEN, SceneMeta, SourceMeta, check_color, check_icon,
};
pub use output::{
    OutputInfo, OutputKind, RecordContainer, RtmpTarget, SecretString,
    rtmp_url_sends_key_in_cleartext,
};
pub use plugin::{
    PLUGIN_SCOPES, PluginClass, PluginDenial, PluginDisabledReason, PluginGrant, PluginScope,
    PluginState, Reach, plugin_classes, plugin_may_bind, plugin_scope, settings_reach,
};
pub use preview::{
    BINARY_HEADER_LEN, BINARY_VERSION, BinaryError, BinaryFrame, BinaryHeader, KeyframeGate,
    PreviewCodec, PreviewDraft, PreviewSettings, StreamKind,
};
pub use query::{
    CaptureTargetInfo, CaptureTargetKind, DeleteTarget, DeviceInfo, ItemInfo, MasterInfo,
    PixelSize, Query, QueryResult, SceneInfo, SceneSummary, SourceInfo, SourceSize,
};
pub use schema::json_schema;
pub use settings::{AssetRoot, Reference, Settings};
pub use transition::{
    CUT, Easing, FADE, Glide, INITIAL_PRESET, ItemEasing, ItemTransition, MAX_DELAY_MS,
    MAX_DURATION_MS, MAX_STAGGER_MS, MOVE, TransitionEffect, TransitionPresetInfo,
    TransitionStarted, TransitionsInfo, builtin_kinds as builtin_transition_kinds,
    check_preset_timing, initial_preset,
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
