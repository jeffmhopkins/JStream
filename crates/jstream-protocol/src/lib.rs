//! The JStream control API: the messages that the engine, the web UI, the CLI and MCP
//! agents all speak. Commands are edits (undoable, batched, revision-checked) or actions
//! (side effects); queries read; events push changes; errors carry stable codes. The JSON
//! Schema is generated from these types. Layer 1: plain data, WASM-safe, no I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-protocol/README.md#purpose, crates/jstream-protocol/README.md#design, docs/adr/0013-command-model.md, docs/adr/0022-item-transform-and-canvas.md, docs/adr/0025-typed-settings-kind-descriptors.md

#![forbid(unsafe_code)]

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
mod id;
mod kind;
mod output;
#[cfg(feature = "fixtures")]
pub mod parity;
mod preview;
mod query;
mod schema;
mod settings;
mod version;

pub use audio::{
    AudioFollow, BusInfo, BusMeter, Loudness, MAX_GAIN_DB, MIN_GAIN_DB, SendInfo, SourceAudio,
    SourceMeter, gain_db_problem,
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
pub use event::{DropCounts, Event, OutputState};
pub use filter::{FilterChain, FilterChains, FilterInfo, FilterOwner, MAX_CHAIN_FILTERS};
pub use finite::Finite32;
pub use geometry::{
    Canvas, CanvasPoint, ContentRect, FitMode, GeometryError, ItemGeometry, LegacyBox, LegacyQuad,
    Placed, PlacementForm, ProjectedQuad, Size, legacy_quad,
};
pub use history::{HistoryCall, HistoryDirection, HistoryOp};
pub use id::{
    BusId, ClientId, DeviceId, EncoderProfileId, FilterId, IdempotencyKey, ItemId, OutputId,
    RequestId, Rev, SceneId, SourceId,
};
pub use kind::{
    EnumOption, KindCategory, KindDescriptor, KindSummary, Media, ParamDescriptor, ParamType,
    VisibleIf, check_asset_path,
};
pub use output::{OutputInfo, OutputKind, RecordContainer, RtmpTarget, SecretString};
pub use preview::{
    BINARY_HEADER_LEN, BINARY_VERSION, BinaryError, BinaryFrame, BinaryHeader, KeyframeGate,
    PreviewCodec, PreviewSettings, StreamKind,
};
pub use query::{
    DeleteTarget, DeviceInfo, ItemInfo, Query, QueryResult, SceneInfo, SceneSummary, SourceInfo,
};
pub use schema::json_schema;
pub use settings::{AssetRoot, Reference, Settings};
pub use version::{PROTOCOL_VERSION, ProtocolVersion};

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

/// R-ARCH-006: every command in the schema is classified as exactly one of edit or
/// action, and every edit is batchable and undoable while no action is.
#[cfg(test)]
#[test]
fn every_command_is_classified() {
    fixtures::check_every_command_is_classified();
}
