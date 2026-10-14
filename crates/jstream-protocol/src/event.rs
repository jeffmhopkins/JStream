//! Events: what the server pushes to every connected client after the handshake.

use crate::id::{ClientId, OutputId, RequestId, Rev};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Something that happened in the engine.
///
/// Serialized with an internal `type` tag holding the event name (`noun.past_tense`).
/// Clients must ignore events they don't know: a newer server may send new ones in a
/// minor version, and they parse as [`Event::Unknown`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Event {
    /// The collection state moved to a new revision: one per applied batch (or undo).
    /// Clients holding a copy of the state re-query, or apply the change if they made it.
    #[serde(rename = "state.changed")]
    StateChanged {
        /// The new revision.
        rev: Rev,
        /// The client whose request caused it.
        client_id: ClientId,
        /// That client's request ID, so it can match the event to its own response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
    },
    /// An output changed state, for example after `output.start`.
    #[serde(rename = "output.state_changed")]
    OutputStateChanged {
        /// The output.
        output_id: OutputId,
        /// Its new state.
        state: OutputState,
    },
    /// This connection's queued events were dropped because the client read them too
    /// slowly: the server keeps a bounded buffer per connection and empties it when it
    /// overflows. `state.changed` events may be among those lost, so the client re-reads
    /// the state with `state.get`, which carries `rev`. Drops that happen before the
    /// notice is sent add up in that one notice. Only events are dropped: every request
    /// still gets its answer. Sent by the WebSocket server, never by an in-process engine.
    #[serde(rename = "events.dropped")]
    EventsDropped {
        /// How many events were dropped.
        count: u64,
    },
    /// An event this version of the protocol doesn't know. Never sent; produced only when
    /// parsing an event from a newer server.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl Event {
    /// The event name, as used in the `type` tag, or `None` for [`Event::Unknown`].
    pub fn name(&self) -> Option<&'static str> {
        match self {
            Event::StateChanged { .. } => Some("state.changed"),
            Event::OutputStateChanged { .. } => Some("output.state_changed"),
            Event::EventsDropped { .. } => Some("events.dropped"),
            Event::Unknown => None,
        }
    }
}

/// The lifecycle state of an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutputState {
    /// Not running.
    Stopped,
    /// Started and connecting or initializing.
    Starting,
    /// Running: streaming, recording or feeding the virtual camera.
    Active,
    /// Lost its connection and is retrying.
    Reconnecting,
    /// Stopping: flushing and disconnecting.
    Stopping,
    /// Stopped because of an error. The error is in the audit log.
    Failed,
}

#[cfg(test)]
mod tests {
    use super::Event;
    use serde_json::json;

    #[test]
    fn unknown_events_from_a_newer_server_parse() {
        let parsed = serde_json::from_value::<Event>(json!({"type": "stats.tick", "fps": 60}));
        assert_eq!(parsed.ok(), Some(Event::Unknown));
    }
}
