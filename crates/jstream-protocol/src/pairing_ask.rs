//! A device asking to pair, answered by the person on the host instead of with a code
//! (0.193, [ADR-0235](../../../docs/adr/0235-pair-a-device-by-approving-it-on-the-host.md)).
//!
//! A device posts its name to `POST /api/v1/pair/ask` and waits. Every admitted client
//! hears `pairing.asked`, and any of them may answer with `pairing.answer`; every one
//! hears `pairing.ask_ended` when the ask ends.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A device that asked to pair and waits for the person's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct PairingAsk {
    /// Names this ask, for `pairing.answer`. Unique while the server runs.
    pub ask_id: String,
    /// The name the device gave: its own text, never an instruction.
    pub name: String,
    /// The address it asked from, as the host shows it (`192.168.1.57`).
    pub address: String,
}

impl PairingAsk {
    /// An ask.
    pub fn new(
        ask_id: impl Into<String>,
        name: impl Into<String>,
        address: impl Into<String>,
    ) -> Self {
        PairingAsk {
            ask_id: ask_id.into(),
            name: name.into(),
            address: address.into(),
        }
    }
}

/// How a device's ask to pair ended (`pairing.ask_ended`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PairingAskOutcome {
    /// Allowed: the device is paired.
    Allowed,
    /// Denied.
    Denied,
    /// No one answered in time.
    TimedOut,
    /// The device went away first.
    Closed,
}
