//! Live values (ADR-0032): what a gesture carries on the wire besides its actions.
//!
//! A gesture overrides parameters of the collection at runtime with `live.set` and ends
//! with a committing batch (`ends_gesture`), a `live.cancel`, or its client's session
//! ending. These are the types its query and events share, and its limits.

use crate::command::Edit;
use crate::id::{ClientId, GestureId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many gestures one client may hold open. Opening one more is `invalid_argument`.
pub const MAX_GESTURES_PER_CLIENT: usize = 16;

/// How many values one `live.set` may carry. More is `invalid_argument`.
pub const MAX_LIVE_VALUES: usize = 256;

/// How a gesture ended, in `live.ended`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LiveOutcome {
    /// A batch with `ends_gesture` applied; its `rev` is in the event.
    Committed,
    /// Its client sent `live.cancel`.
    Cancelled,
    /// Its client's session ended.
    Expired,
}

impl LiveOutcome {
    /// The outcome as it is written on the wire (`committed`).
    pub fn name(self) -> &'static str {
        match self {
            LiveOutcome::Committed => "committed",
            LiveOutcome::Cancelled => "cancelled",
            LiveOutcome::Expired => "expired",
        }
    }
}

/// One open gesture, in a `live.list` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GestureInfo {
    /// The gesture, as its client named it.
    pub gesture_id: GestureId,
    /// The client that opened it.
    pub client_id: ClientId,
    /// The value of each parameter it holds now, as setter edits. A parameter another
    /// gesture took over is not among them.
    pub values: Vec<Edit>,
}

/// Whether `live.set` accepts `edit` as a value: a setter of a parameter the real-time
/// threads take from a parameter channel, with no snapshot rebuild. Today that is an
/// item's placement (`item.set_transform`, `item.set_geometry`) and its crop
/// (`item.set_crop`).
pub fn is_live_adjustable(edit: &Edit) -> bool {
    matches!(
        edit,
        Edit::ItemSetTransform { .. } | Edit::ItemSetGeometry { .. } | Edit::ItemSetCrop { .. }
    )
}
