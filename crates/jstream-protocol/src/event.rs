//! Events: what the server pushes to every connected client after the handshake.

use crate::audio::{BusMeter, SourceMeter};
use crate::command::Edit;
use crate::history::HistoryDirection;
use crate::id::{ClientId, GestureId, OutputId, PluginId, RequestId, Rev, SceneId, SourceId};
use crate::live::LiveOutcome;
use crate::plugin::{PluginDisabledReason, PluginState};
use crate::query::PixelSize;
use crate::transition::TransitionStarted;
use crate::youtube::{AudienceStatsInfo, HealthIssue, StreamHealth, StreamStatus, YoutubeAuth};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Something that happened in the engine.
///
/// Serialized with an internal `type` tag holding the event name (`noun.past_tense`).
/// Clients must ignore events they don't know: a newer server may send new ones in a
/// minor version, and they parse as [`Event::Unknown`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// A source's size changed (ADR-0022 §4, 0.40), including when it became known (its
    /// first frame) or unknown (it stopped, failed, or was deleted). Runtime state, so it
    /// carries no `rev`. The size is the one `source.sizes` reports.
    #[serde(rename = "source.size_changed")]
    SourceSizeChanged {
        /// The source.
        source_id: SourceId,
        /// Its new size. Omitted when it has no video frame now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<PixelSize>,
    },
    /// The scene on program or on preview changed: after a `scene.switch` or a
    /// `studio.set_preview`, or after a batch, undo or redo deleted the scene (it gives
    /// way to the first remaining scene) or created the collection's first scene (it goes
    /// on program and on preview). Studio state is runtime state (ADR-0030), so the event
    /// carries no `rev`; a `state.changed` for the same edits comes first.
    #[serde(rename = "studio.changed")]
    StudioChanged {
        /// The scene now on program. Omitted when the collection has no scene left.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        program_scene: Option<SceneId>,
        /// The scene now on preview. Omitted when the collection has no scene left, and
        /// by a server older than 0.30.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview_scene: Option<SceneId>,
        /// The transition a `scene.switch` started: the preset the engine chose and how
        /// long it runs. Omitted when program did not change by a switch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition: Option<TransitionStarted>,
        /// The client whose request caused it.
        client_id: ClientId,
        /// That client's request ID, so it can match the event to its own response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
    },
    /// **Deprecated** (ADR-0080), removed in 1.0. JStream has no privacy
    /// screen, so the engine never sends it.
    #[serde(rename = "privacy.changed")]
    PrivacyChanged {
        /// Whether the privacy screen now covers the outputs.
        on: bool,
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
        /// What the scene filter chains and item effects add to each frame: their
        /// offscreen layers, GPU passes and the layers' texture bytes, as the current
        /// render plan schedules them (ADR-0065 §7; 0.54). An older engine leaves it
        /// out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chains: Option<ChainCost>,
    },
    /// A gesture's live values changed (ADR-0032): the newest value of each parameter
    /// that changed since the gesture's last `live.changed`, as setter edits. The engine
    /// coalesces them per gesture to at most 30 a second, so a slow client gets fewer,
    /// newer values, never a backlog; the last values before the gesture ends are always
    /// sent. Live values are runtime state: no `rev` changes.
    #[serde(rename = "live.changed")]
    LiveChanged {
        /// The gesture.
        gesture_id: GestureId,
        /// The client that holds it.
        client_id: ClientId,
        /// The parameters' new values.
        values: Vec<Edit>,
    },
    /// A gesture ended, and its parameters show their collection values again. For
    /// `committed`, the committing batch's `state.changed` comes first, and `rev` is the
    /// revision it produced.
    #[serde(rename = "live.ended")]
    LiveEnded {
        /// The gesture.
        gesture_id: GestureId,
        /// The client that held it.
        client_id: ClientId,
        /// How it ended.
        outcome: LiveOutcome,
        /// The committing batch's revision, for `committed` only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<Rev>,
    },
    /// The engine's YouTube sign-in state changed: a sign-in started, finished or failed,
    /// the user signed out, or Google stopped accepting the account's token.
    #[serde(rename = "youtube.status_changed")]
    YoutubeStatusChanged {
        /// The new state.
        auth: YoutubeAuth,
    },
    /// What YouTube says about the stream a running `youtube` output sends to changed
    /// (ADR-0058). The engine polls the stream's health while the output runs and sends
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
    /// A running `youtube` output's audience (ADR-0041 §3, 0.38): sent when a reading
    /// differs from the last one, and once on the first. The engine reads it about once
    /// a minute while the output runs, less often or not at all when the day's quota
    /// runs low (the `youtube.stats` query says which).
    #[serde(rename = "youtube.stats")]
    YoutubeStats {
        /// The output, its broadcast and the counts.
        #[serde(flatten)]
        stats: AudienceStatsInfo,
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
    /// A plugin was installed, enabled or disabled (0.35, ADR-0069). The engine disables
    /// a plugin that runs past a limit or traps, says why, and never re-enables it by
    /// itself.
    #[serde(rename = "plugin.state_changed")]
    PluginStateChanged {
        /// The plugin.
        plugin_id: PluginId,
        /// Whether it runs.
        state: PluginState,
        /// Why the engine disabled it. Omitted when it is enabled, or when a person
        /// disabled it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<PluginDisabledReason>,
        /// The name the plugin's manifest gives itself: the plugin's own text, never an
        /// instruction (ADR-0059). Omitted when it gives none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_name: Option<String>,
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
            Event::SourceSizeChanged { .. } => Some("source.size_changed"),
            Event::EventsDropped { .. } => Some("events.dropped"),
            Event::StatsTick { .. } => Some("stats.tick"),
            Event::LiveChanged { .. } => Some("live.changed"),
            Event::LiveEnded { .. } => Some("live.ended"),
            Event::YoutubeStatusChanged { .. } => Some("youtube.status_changed"),
            Event::YoutubeHealthChanged { .. } => Some("youtube.health_changed"),
            Event::YoutubeStats { .. } => Some("youtube.stats"),
            Event::MetersTick { .. } => Some("meters.tick"),
            Event::PluginStateChanged { .. } => Some("plugin.state_changed"),
            Event::StudioChanged { .. } => Some("studio.changed"),
            Event::PrivacyChanged { .. } => Some("privacy.changed"),
            Event::Unknown => None,
        }
    }
}

/// What the video chains of scenes and items add to each frame the engine draws
/// (ADR-0065 §7), as the current render plan schedules them: a gauge, not a running
/// total.
///
/// A scene with an enabled video filter is composited once per frame into a
/// canvas-sized layer, however many places show it, and each enabled filter then draws
/// one pass into a layer of its own. An item with an enabled effect is drawn into one
/// layer the size of its box plus the effects' outset, and each enabled effect then
/// draws one pass into a layer of its own. An item placed in two scenes counts once for
/// each. A filter of a kind the engine doesn't draw is bypassed and not counted. Every
/// layer is `Rgba16Float`, 8 bytes a pixel. Not counted: a source's own filters
/// (ADR-0026), the scenes a `scene_mask` reads, and the canvas-sized layer a nested
/// scene without a chain is drawn into for an item with effects that shows it (it takes
/// the place of the layer the scene would be drawn into anyway).
///
/// A missing field parses as 0, so a field added later can be left out by an older
/// sender (R-API-001). Unknown fields are ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ChainCost {
    /// Scenes whose video chain runs each frame.
    pub scene_chains: u32,
    /// The offscreen layers those chains draw into: each scene's composite and one per
    /// filter.
    pub scene_layers: u32,
    /// The GPU passes those chains draw: each scene's composite and one per filter.
    pub scene_passes: u32,
    /// Items whose effects run each frame, once for each scene that draws the item.
    pub item_effects: u32,
    /// The offscreen layers those effects draw into: each item's box and one per
    /// effect.
    pub item_layers: u32,
    /// The GPU passes those effects draw: each item's box and one per effect.
    pub item_passes: u32,
    /// The texture memory of every layer above, in bytes.
    pub layer_bytes: u64,
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
            chains: Some(super::ChainCost {
                scene_chains: 1,
                scene_layers: 2,
                scene_passes: 2,
                item_effects: 1,
                item_layers: 3,
                item_passes: 3,
                layer_bytes: 99_532_800,
            }),
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
                },
                "chains": {
                    "scene_chains": 1,
                    "scene_layers": 2,
                    "scene_passes": 2,
                    "item_effects": 1,
                    "item_layers": 3,
                    "item_passes": 3,
                    "layer_bytes": 99_532_800u64,
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
                chains: None,
            }
        );
    }

    /// A tick with a partial `chains` parses, missing counts reading as 0, and a tick
    /// without one leaves it out (R-API-001).
    #[test]
    fn stats_tick_chain_cost_is_optional() {
        let parsed = serde_json::from_value::<Event>(json!({
            "type": "stats.tick",
            "seq": 5,
            "drops": {},
            "chains": {"item_effects": 2, "future_layers": 1}
        }))
        .expect("parses");
        assert_eq!(
            parsed,
            Event::StatsTick {
                seq: 5,
                drops: super::DropCounts::default(),
                chains: Some(super::ChainCost {
                    item_effects: 2,
                    ..super::ChainCost::default()
                }),
            }
        );
        let without = Event::StatsTick {
            seq: 6,
            drops: super::DropCounts::default(),
            chains: None,
        };
        let value = serde_json::to_value(&without).expect("serializes");
        assert_eq!(value.get("chains"), None);
    }
}
