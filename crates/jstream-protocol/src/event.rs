//! Events: what the server pushes to every connected client after the handshake.

use crate::audio::{BusMeter, SourceMeter};
use crate::history::HistoryDirection;
use crate::id::{ClientId, OutputId, RequestId, Rev, SceneId};
use crate::youtube::{HealthIssue, StreamHealth, StreamStatus, YoutubeAuth};
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
        /// `undo` or `redo` when a history call made the change; absent for a batch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<HistoryDirection>,
    },
    /// An output changed state, for example after `output.start`.
    #[serde(rename = "output.state_changed")]
    OutputStateChanged {
        /// The output.
        output_id: OutputId,
        /// Its new state.
        state: OutputState,
    },
    /// The scene on program changed: after a `scene.switch`, or after a batch, undo or
    /// redo deleted the program scene (program moves to the first remaining scene) or
    /// created the collection's first scene (it goes on program). Studio state is runtime state
    /// (ADR-0030), so the event carries no `rev`; a `state.changed` for the same edits
    /// comes first.
    #[serde(rename = "studio.changed")]
    StudioChanged {
        /// The scene now on program. Omitted when the collection has no scene left.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        program_scene: Option<SceneId>,
        /// The client whose request caused it.
        client_id: ClientId,
        /// That client's request ID, so it can match the event to its own response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
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
    /// A sample of the pipeline's drop counters (R-RT-004).
    ///
    /// `drops` is a running total since the engine started, not the count since the
    /// previous tick. A client that misses one subtracts the last tick it did see.
    /// `seq` starts at 1 and increases by one per sample, so a gap means a missed tick
    /// and not a reset. The engine sends it when its control thread asks, about once a
    /// second once the media pipeline is running. Until event subscriptions exist, every
    /// client receives it.
    #[serde(rename = "stats.tick")]
    StatsTick {
        /// Monotonic sample number, starting at 1.
        seq: u64,
        /// Running drop totals.
        drops: DropCounts,
    },
    /// The engine's YouTube sign-in state changed: a sign-in started, finished or failed,
    /// the user signed out, or Google stopped accepting the account's token.
    #[serde(rename = "youtube.status_changed")]
    YoutubeStatusChanged {
        /// The new state.
        auth: YoutubeAuth,
    },
    /// What YouTube says about the stream a running `youtube` output sends to changed
    /// (ADR-0041). The engine polls the stream's health while the output runs and sends
    /// this when the status, the health or the issues differ from the last report, and
    /// once on the first reading.
    #[serde(rename = "youtube.health_changed")]
    YoutubeHealthChanged {
        /// The output.
        output_id: OutputId,
        /// The broadcast it sends to.
        broadcast_id: String,
        /// The stream bound to that broadcast.
        stream_id: String,
        /// What YouTube says the stream is doing.
        status: StreamStatus,
        /// How healthy what it receives is.
        health: StreamHealth,
        /// The problems YouTube found. Omitted when there are none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        issues: Vec<HealthIssue>,
    },
    /// The mixer's levels (ADR-0028): sent about ten times a second while the mixer runs
    /// (an output is running), and never while it doesn't. Runtime state: a client that
    /// misses one just shows the next. Until event subscriptions exist, every client
    /// receives it.
    #[serde(rename = "meters.tick")]
    MetersTick {
        /// Each audio source with a mixer channel, ordered by ID.
        #[serde(default)]
        sources: Vec<SourceMeter>,
        /// Each bus, ordered by ID, the master among them.
        #[serde(default)]
        buses: Vec<BusMeter>,
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
            Event::StatsTick { .. } => Some("stats.tick"),
            Event::YoutubeStatusChanged { .. } => Some("youtube.status_changed"),
            Event::YoutubeHealthChanged { .. } => Some("youtube.health_changed"),
            Event::MetersTick { .. } => Some("meters.tick"),
            Event::StudioChanged { .. } => Some("studio.changed"),
            Event::Unknown => None,
        }
    }
}

/// Running totals of frames and packets the pipeline has dropped since the engine
/// started (R-RT-004).
///
/// Each field sums one kind of drop across every stage that counts it. A stage that
/// does not count that kind contributes nothing. Totals only grow: a client computes
/// an interval by subtracting the previous tick.
///
/// Silence a stage inserts to cover a gap (an audio underrun, a device gap, encoder
/// gap-fill) is not a drop and is not included.
///
/// A missing field parses as 0, so a counter added in a later minor version can be left
/// out by an older sender (R-API-001). Unknown fields are ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DropCounts {
    /// Video frames a capture mailbox replaced before the reader took them.
    pub capture_frames: u64,
    /// Audio frames dropped before the mixer consumed them: a device ring that
    /// overwrote unread frames, frames that arrived after the mixer had passed them,
    /// and frames the mixer skipped because it fell behind the clock.
    pub audio_frames: u64,
    /// Render ticks the render loop skipped to catch up with the media clock.
    pub render_ticks: u64,
    /// Video frames dropped before an encoder accepted them (its input queue was full,
    /// or the encoder was busy).
    pub encode_frames: u64,
    /// Audio blocks dropped before an encoder accepted them (its input queue was full,
    /// or the encoder was busy).
    pub encode_blocks: u64,
    /// Encoded packets dropped handing a stream to an output: a subscriber that fell
    /// behind, or the output's own queue.
    pub output_packets: u64,
}

impl DropCounts {
    /// The field-wise sum, saturating at [`u64::MAX`] rather than wrapping.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            capture_frames: self.capture_frames.saturating_add(other.capture_frames),
            audio_frames: self.audio_frames.saturating_add(other.audio_frames),
            render_ticks: self.render_ticks.saturating_add(other.render_ticks),
            encode_frames: self.encode_frames.saturating_add(other.encode_frames),
            encode_blocks: self.encode_blocks.saturating_add(other.encode_blocks),
            output_packets: self.output_packets.saturating_add(other.output_packets),
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
        let parsed = serde_json::from_value::<Event>(json!({"type": "future.event", "fps": 60}));
        assert_eq!(parsed.ok(), Some(Event::Unknown));
    }

    #[test]
    fn stats_tick_roundtrips_its_running_totals() {
        let event = Event::StatsTick {
            seq: 3,
            drops: super::DropCounts {
                capture_frames: 2,
                audio_frames: 5,
                render_ticks: 1,
                encode_frames: 4,
                encode_blocks: 6,
                output_packets: 7,
            },
        };
        let value = serde_json::to_value(&event).expect("serializes");
        assert_eq!(
            value,
            json!({
                "type": "stats.tick",
                "seq": 3,
                "drops": {
                    "capture_frames": 2,
                    "audio_frames": 5,
                    "render_ticks": 1,
                    "encode_frames": 4,
                    "encode_blocks": 6,
                    "output_packets": 7,
                }
            })
        );
        let parsed = serde_json::from_value::<Event>(value).expect("parses");
        assert_eq!(parsed, event);
    }

    /// A tick from a peer with fewer (or more) counters than this version still parses:
    /// missing ones read as 0 and unknown ones are ignored (R-API-001).
    #[test]
    fn stats_tick_with_missing_or_extra_counters_parses() {
        let parsed = serde_json::from_value::<Event>(json!({
            "type": "stats.tick",
            "seq": 4,
            "drops": {"capture_frames": 2, "future_frames": 9}
        }))
        .expect("parses");
        assert_eq!(
            parsed,
            Event::StatsTick {
                seq: 4,
                drops: super::DropCounts {
                    capture_frames: 2,
                    ..super::DropCounts::default()
                },
            }
        );
    }
}
