//! Live data (0.133,
//! [ADR-0171](../../../docs/adr/0171-live-data-from-a-daw-drives-scenes.md) decision 2):
//! each JTunnel sender's newest state message per key, for a sender whose "Use this
//! sender's state" is on. A DAW's JTunnel sends what it is doing as JSON messages under
//! keys such as `transport` or `track/3`; `data.list` names the senders and their keys,
//! and `data.get` reads one message. Runtime state, never saved and never an edit; a
//! client only reads it, and nothing sets it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One sender with live data, as `data.list` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DataInputInfo {
    /// The sender's name on this machine's accepted list, as `data.get`'s `input` names
    /// it.
    pub input: String,
    /// Its keys with a current message, in order: 1 to 63 of ASCII letters, digits,
    /// `/`, `_`, `.` and `-`, chosen by the sender.
    pub remote_keys: Vec<String>,
}
