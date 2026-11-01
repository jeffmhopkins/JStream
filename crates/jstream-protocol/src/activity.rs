//! The activity log (0.51, ADR-0083): what each client did, as the engine's audit log
//! records it, with a sentence the engine writes for people to read.

use crate::id::ClientId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many entries the engine keeps, and the most one `activity.list` answers.
pub const MAX_ACTIVITY_ENTRIES: u32 = 100;

/// One executed batch, undo, redo or action, or a gesture that ended without a commit:
/// an entry of the engine's audit log, as `activity.list` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityEntry {
    /// Its number: 1 for the engine's first entry, and one more for each after it.
    pub seq: u64,
    /// When the call finished, in milliseconds since the Unix epoch (UTC).
    pub time_ms: u64,
    /// The client that sent it. Numbered per engine run.
    pub client_id: ClientId,
    /// The name the client gave in its latest `hello` (`claude-desktop`), or
    /// `device:<name>` for a paired device. Chosen by the client, so it says who claims
    /// to act, not who proved it.
    pub client_name: String,
    /// What it called, as the audit log names it: `batch`, `history.undo`,
    /// `history.redo`, an action's name (`scene.switch`), or `live.cancel` or
    /// `live.ended` for a gesture that ended without a commit.
    pub call: String,
    /// One English sentence saying what it did ("Switched to “Desktop + Cam”", "Mic
    /// gain −1.5 → −3.0 dB"), written from the collection just before the call ran.
    /// It names objects and gives numeric, on/off and enum values only: never a
    /// source's or filter's settings, an output's target, or a stream key.
    pub summary: String,
    /// The protocol error code when the call failed (`rev_conflict`). Omitted when it
    /// succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
