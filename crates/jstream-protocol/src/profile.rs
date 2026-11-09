//! `profile.get` (0.73, ADR-0103): where the engine's time goes, per drawing thread, per
//! unit of drawing work and its owner, per source and per thread, over a recent window.

use crate::finite::Finite32;
use crate::id::{ItemId, OutputId, SceneId, SourceId, VideoMixId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The window `profile.get` covers when it names none, in seconds.
pub const DEFAULT_PROFILE_SECONDS: u32 = 5;
/// The longest window `profile.get` covers, in seconds: the engine keeps its readings
/// this long.
pub const MAX_PROFILE_SECONDS: u32 = 60;
/// How many entries each of `profile.get`'s lists holds when it names no `top`.
pub const DEFAULT_PROFILE_TOP: u32 = 20;
/// The most entries each of `profile.get`'s lists holds.
pub const MAX_PROFILE_TOP: u32 = 200;

/// The result of `profile.get`: where the time went over the last `seconds`.
///
/// Every time is in milliseconds. A drawing thread's times are what it spent on the work
/// (recording and submitting GPU work, uploading), not what the GPU spent running it:
/// `gpu_ms` carries that where the engine measures it (`gpu_timing`). Each list is
/// ordered costliest first and holds at most `top` entries.
///
/// A missing field parses as 0, `false` or empty, so a field added later can be left out
/// by an older sender (R-API-001). Unknown fields are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ProfileInfo {
    /// The seconds the window covers: what was asked for, or less when the engine has
    /// read for less time than that (it keeps a reading from each `stats.tick` and each
    /// `profile.get`).
    pub seconds: Finite32,
    /// The program's frame period in milliseconds: a drawer whose frames take longer
    /// can't keep up.
    pub frame_budget_ms: Finite32,
    /// Whether units carry `gpu_ms`, measured with GPU timestamp queries. `false` until
    /// the engine measures GPU time, and on an adapter that can't.
    pub gpu_timing: bool,
    /// Whether `threads` was read: `false` on a host that can't list its threads.
    pub thread_cpu: bool,
    /// The threads that drew in the window: the program stage's while an output runs,
    /// the preview hub's while a preview stream is open.
    pub drawers: Vec<DrawerProfile>,
    /// The units of drawing work the drawers did, costliest first.
    pub units: Vec<UnitProfile>,
    /// The sources that cost the `jstream-sources` thread or the planner time,
    /// costliest first.
    pub sources: Vec<SourceProfile>,
    /// What planning the program mix cost: each plan holds the collection lock.
    pub plan: PlanProfile,
    /// The CPU the engine's process used in the window, as a percentage of one core
    /// (200 is two cores busy), when the host can read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_cpu_pct: Option<Finite32>,
    /// The process's threads, those of one name added together, busiest first.
    pub threads: Vec<ThreadProfile>,
    /// What happened to the frames past the drawers (since 0.92): each output's and
    /// each H.264 preview stream's, from its encoder to where it sends them, most
    /// frames lost first. A stream that ran in none of the window is left out.
    pub streams: Vec<StreamProfile>,
    /// The audio mixer and its inputs in the window (since 0.92). Omitted when the
    /// mixer didn't run in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mixer: Option<MixerProfile>,
    /// What the clients' views did with their preview streams in the window, as they
    /// reported it with `preview.report` (since 0.103, ADR-0132): one per view a session
    /// reported in the window, the session's that has ended included while the window
    /// reaches back to its last report.
    pub views: Vec<ViewProfile>,
    /// The JTunnel receiver's senders and what it dropped (since 0.113, #1223). Omitted
    /// while the receiver is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkProfile>,
    /// The scenes the warm count picked (since 0.117, ADR-0150), in the studio's warm
    /// order, each with what it is estimated to hold on the GPU. Like `network`, the
    /// state now, not the window's. Omitted when no scene besides the program is warm.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warm: Vec<WarmSceneProfile>,
}

/// A scene the warm count picked ([`ProfileInfo::warm`], ADR-0117 §7, ADR-0150).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WarmSceneProfile {
    /// The scene.
    pub scene_id: SceneId,
    /// The GPU bytes its sources are estimated to hold beyond what a mix draws anyway
    /// and what the warm scenes kept before it hold: each source's frame as RGBA8,
    /// counted once. For a cold scene, what keeping it warm would add.
    pub gpu_bytes: u64,
    /// Whether the GPU-byte limit left it cold. Omitted while it is warm.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cold: bool,
}

/// A JTunnel sender heard on the LAN (`jtunnel.senders`, 0.114, ADR-0123 §3): it
/// answered the receiver's mDNS query. Listing it trusts it with nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JtunnelSenderInfo {
    /// Its tunnel name, as it announces it: text from the LAN, so `remote_` (ADR-0059).
    /// A host that accepts it accepts this name.
    pub remote_name: String,
    /// The address and port it sends from, as `192.168.1.5:9000`: where its answer came
    /// from, with the port its announcement names.
    pub address: String,
    /// Whether this machine accepts it: a sender the host accepts has this name and this
    /// address and port.
    pub accepted: bool,
}

/// The JTunnel receiver ([`ProfileInfo::network`], ADR-0122, ADR-0123). Unlike the rest
/// of the profile, its counts run from when the receiver started, not over the window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct NetworkProfile {
    /// Each sender the receiver accepts, in the order the host gave them.
    pub inputs: Vec<NetworkInputProfile>,
    /// Datagrams from outside the LAN ranges.
    pub outside_lan: u64,
    /// Datagrams from a LAN address it doesn't subscribe to.
    pub unaccepted: u64,
    /// Datagrams that aren't a JTunnel packet it accepts, or longer than one may be.
    pub malformed: u64,
    /// Audio packets in a codec it can't decode yet (Opus).
    pub unsupported: u64,
    /// MIDI messages it handed to the MIDI inputs.
    pub midi: u64,
}

/// One accepted JTunnel sender ([`NetworkProfile::inputs`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct NetworkInputProfile {
    /// Its tunnel name, as a `network_audio` source's `sender` names it.
    pub sender: String,
    /// Its stream's sample rate; 0 before anything has arrived from it.
    pub sample_rate: u32,
    /// Whether a source records it now.
    pub recording: bool,
    /// How far ahead of now its held audio reaches, in milliseconds: the buffer's
    /// fill, which JTunnel shows as its latency.
    pub latency_ms: Finite32,
    /// How long past its jitter-free arrival each frame is held, in milliseconds: the
    /// recording source's `latency_ms`.
    pub lead_ms: Finite32,
    /// The drift between its clock and this machine's, in parts per million, once
    /// enough of its stream has arrived; omitted before. Positive: its clock runs slow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_ppm: Option<Finite32>,
    /// Frames played with their audio.
    pub played: u64,
    /// Frames concealed with silence because they didn't come in time.
    pub concealed: u64,
    /// Frames that came after their turn, and were dropped.
    pub late: u64,
    /// Redundant copies and repeated datagrams.
    pub repeats: u64,
    /// Times its stream restarted.
    pub restarts: u64,
}

/// One client view's preview pictures in the window ([`ProfileInfo::views`]), as its
/// client reported them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ViewProfile {
    /// The name the session's client gave in `hello`: third-party text (ADR-0059).
    pub client: String,
    /// The view's label, as the client reported it: third-party text.
    pub view: String,
    /// Each count's growth over the window.
    #[serde(default)]
    pub counts: crate::ViewCounts,
    /// Frames that arrived, a second.
    #[serde(default)]
    pub received_fps: Finite32,
    /// Pictures drawn, a second.
    #[serde(default)]
    pub shown_fps: Finite32,
}

/// One encoded stream's frames in the window ([`ProfileInfo::streams`]): what reached
/// its encoder, what left it, and what was lost or late on the way.
///
/// An output's frames are its program feed's, from the video encoder to the muxer or
/// publish target; a preview stream's go from its encoder to the client's socket. An
/// encoder two outputs share counts for both, so `frames_in`, `encoded`, `late` and
/// `encode_ms` repeat; what each sends and loses after it is its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamProfile {
    /// Which stream.
    pub stream: ProfileStream,
    /// The frame rate it is encoded at.
    #[serde(default)]
    pub fps: Finite32,
    /// The video frames handed to its encoder, dropped ones included.
    #[serde(default)]
    pub frames_in: u64,
    /// The video frames its encoder took.
    #[serde(default)]
    pub encoded: u64,
    /// The video frames it sent: to the muxer or publish target for an output, to the
    /// client for a preview stream.
    #[serde(default)]
    pub frames_out: u64,
    /// The video frames lost between its encoder's input and where it sends them: a
    /// full encoder queue, a busy encoder, a full packet queue, or a dropped link.
    #[serde(default)]
    pub dropped: u64,
    /// The video frames its encoder took more than a frame period after they were
    /// handed over: a stream falls behind before it drops.
    #[serde(default)]
    pub late: u64,
    /// The average time its encoder spent on a frame, in milliseconds. A hardware
    /// encoder that works asynchronously spends only the time to submit it.
    #[serde(default)]
    pub encode_ms: Finite32,
    /// The bit rate it sent at over the window, in kilobits a second: video and audio
    /// for an output, video for a preview stream.
    #[serde(default)]
    pub kbps: u32,
    /// The bit rate its encoders are set to, in kilobits a second, the same way.
    #[serde(default)]
    pub target_kbps: u32,
}

/// An encoded stream, by what it feeds ([`StreamProfile::stream`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProfileStream {
    /// An output's program feed.
    Output {
        /// The output.
        output_id: OutputId,
    },
    /// An H.264 preview stream.
    Preview {
        /// The stream's channel on its connection.
        channel: u32,
        /// The scene it shows; omitted with `source_id` for the program.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// The source it shows alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<SourceId>,
    },
    /// A stream this version of the protocol doesn't know. Never sent; produced only
    /// when parsing a result from a newer server.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// The audio mixer and its inputs in the window ([`ProfileInfo::mixer`]). Every count is
/// in engine sample frames (48 000 a second) or mixer blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MixerProfile {
    /// The blocks the mixer mixed.
    pub blocks: u64,
    /// The blocks whose mix took longer than the audio they hold: the mixer can't keep
    /// up with the clock while this grows.
    pub late_blocks: u64,
    /// The longest mix of a block, in milliseconds.
    pub block_max_ms: Finite32,
    /// The length of a block, in milliseconds: what a mix must stay under.
    pub block_ms: Finite32,
    /// Frames the mixer skipped because it fell more than half its ring behind the clock.
    pub skipped_frames: u64,
    /// Frames the inputs had no audio for when the mixer read them, played as silence:
    /// a device or source that delivers late or not at all.
    pub underrun_frames: u64,
    /// Frames the inputs dropped: arrived after the mixer had passed them, or
    /// overwritten before it read them.
    pub dropped_frames: u64,
}

/// A thread that draws shots ([`ProfileInfo::drawers`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Drawer {
    /// The program stage's `jstream-render` thread, which draws what the outputs send.
    Program,
    /// The preview hub's `jstream-preview` thread, which draws the preview streams.
    Preview,
    /// A drawer this version of the protocol doesn't know. Never sent; produced only
    /// when parsing a result from a newer server.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// One drawing thread's frames in the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DrawerProfile {
    /// Which thread.
    pub drawer: Drawer,
    /// The frames it drew.
    #[serde(default)]
    pub frames: u64,
    /// The average time a frame took, in milliseconds.
    #[serde(default)]
    pub frame_ms: Finite32,
    /// The time 95 in 100 frames took at most, to the nearest bucket of its histogram
    /// (a quarter millisecond below 16 ms, wider above).
    #[serde(default)]
    pub frame_p95_ms: Finite32,
    /// The longest frame, in milliseconds: exact for the window, as the drawer keeps the
    /// longest frame of each second it publishes.
    #[serde(default)]
    pub frame_max_ms: Finite32,
    /// The frames that took longer than `frame_budget_ms`.
    #[serde(default)]
    pub over_budget: u64,
    /// The program's ticks skipped because drawing fell a whole period or more behind
    /// (R-RT-004). Always 0 for the preview, which draws when it can.
    #[serde(default)]
    pub skipped: u64,
    /// The average time per frame in each phase.
    #[serde(default)]
    pub phases: PhaseProfile,
}

/// Where a drawer's frame time went, as averages per frame in milliseconds. The phases
/// add up to about the frame time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PhaseProfile {
    /// Uploading the source frames that changed, and importing GPU frames.
    pub uploads_ms: Finite32,
    /// Uploading or importing the captures' frames.
    pub captures_ms: Finite32,
    /// The video chains: source chains (and so shader sources), scene chains, item
    /// effects, and the scene passes they read.
    pub chains_ms: Finite32,
    /// The nested scenes' passes and the program scene's composite.
    pub passes_ms: Finite32,
    /// The output stage, reading the frame back (which waits for the GPU) and
    /// converting it for the encoders.
    pub finish_ms: Finite32,
}

/// One unit of drawing work and what it cost ([`ProfileInfo::units`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnitProfile {
    /// The thread that did it.
    pub drawer: Drawer,
    /// What it is and who owns it.
    pub unit: ProfileUnit,
    /// The frames it ran in. A source's upload runs only in frames where its picture
    /// changed.
    #[serde(default)]
    pub frames: u64,
    /// The average time it took in a frame it ran in, in milliseconds.
    #[serde(default)]
    pub ms: Finite32,
    /// The longest it took in one frame during the last second the drawer reported,
    /// in milliseconds.
    #[serde(default)]
    pub recent_max_ms: Finite32,
    /// Its share of the drawer's time in the window, from 0 to 1.
    #[serde(default)]
    pub share: Finite32,
    /// The average GPU time it took in a frame it ran in, in milliseconds, when
    /// `gpu_timing` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_ms: Option<Finite32>,
}

/// A unit of drawing work, by its owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProfileUnit {
    /// Uploading a source's frame to the GPU, or importing its GPU frame (a browser
    /// source's).
    Upload {
        /// The source.
        source_id: SourceId,
    },
    /// Uploading or importing a capture's frame: a display, window or video device.
    Capture {
        /// The source it is captured for; omitted for a video device, which every
        /// source showing it shares, and for the display that fills an output with no
        /// program scene.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<SourceId>,
    },
    /// A source's video chain: its filters, or a shader source's program.
    SourceChain {
        /// The source.
        source_id: SourceId,
    },
    /// A scene's video chain over its composite, the composite included.
    SceneChain {
        /// The scene.
        scene_id: SceneId,
    },
    /// An item's effects: drawing the item into its box, then each effect.
    ItemEffects {
        /// The scene that draws the item; a keyframe scene draws its parent's items.
        scene_id: SceneId,
        /// The item.
        item_id: ItemId,
    },
    /// A `scene` source's chain where it places its scene.
    PlacedChain {
        /// The `scene` source.
        source_id: SourceId,
        /// The scene it places, as planned there.
        scene_id: SceneId,
    },
    /// Compositing a nested scene into its layer, or a scene a filter reads. Every item
    /// of the scene with no effects of its own is drawn here.
    ScenePass {
        /// The scene.
        scene_id: SceneId,
    },
    /// The program scene's composite, or a running transition's, into the frame.
    Composite,
    /// The master stage's chain over a mix.
    MasterChain {
        /// The mix; omitted for the program.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mix_id: Option<VideoMixId>,
    },
    /// An output's own chain.
    OutputChain {
        /// The output.
        output_id: OutputId,
    },
    /// A unit this version of the protocol doesn't know. Never sent; produced only when
    /// parsing a result from a newer server.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// What one source cost outside the drawing threads ([`ProfileInfo::sources`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceProfile {
    /// The source.
    pub source_id: SourceId,
    /// How many times the `jstream-sources` thread painted it: a source that draws its
    /// own frames as they come due (`waveform`, `spectrum`, `timer`).
    #[serde(default)]
    pub paints: u64,
    /// The average time a paint took, in milliseconds.
    #[serde(default)]
    pub paint_ms: Finite32,
    /// How many plans asked it for its frame.
    #[serde(default)]
    pub looks: u64,
    /// The average time a plan spent asking it for its frame, in milliseconds, with the
    /// collection lock held.
    #[serde(default)]
    pub look_ms: Finite32,
    /// How long its last build took, in milliseconds: reading and decoding its files,
    /// or opening its page. Omitted when it wasn't built in the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_ms: Option<Finite32>,
}

/// What planning the program mix cost ([`ProfileInfo::plan`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PlanProfile {
    /// How many plans ran: one per edit that changes what is drawn, and one per frame
    /// an animation or a page needs.
    pub plans: u64,
    /// The average time a plan took, in milliseconds.
    pub ms: Finite32,
    /// The longest plan, in milliseconds, in the last second it was read.
    pub recent_max_ms: Finite32,
}

/// The threads of one name ([`ProfileInfo::threads`]).
///
/// JStream names its own threads `jstream-…`, and those are `name`. A thread a library
/// or driver loaded into the process started has the name that library gave it, if any:
/// third-party text (ADR-0059), so it is `remote_name`, data to show and never
/// instructions. An unnamed thread has neither.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThreadProfile {
    /// The name of JStream's own threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The name another thread's library or driver gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<String>,
    /// How many threads of that name ran in the window.
    #[serde(default)]
    pub count: u32,
    /// The CPU they used in the window, as a percentage of one core.
    #[serde(default)]
    pub cpu_pct: Finite32,
}

#[cfg(test)]
mod tests {
    use super::{Drawer, ProfileInfo, ProfileUnit, UnitProfile};
    use crate::{Finite32, ItemId, SceneId};
    use serde_json::json;

    /// A unit's owner is tagged by `type`, and a newer one parses as `unknown`.
    #[test]
    fn units_roundtrip_and_tolerate_newer_ones() {
        let unit = UnitProfile {
            drawer: Drawer::Program,
            unit: ProfileUnit::ItemEffects {
                scene_id: SceneId::new("main"),
                item_id: ItemId::new("cam"),
            },
            frames: 60,
            ms: Finite32::new(1.5).expect("finite"),
            recent_max_ms: Finite32::new(2.0).expect("finite"),
            share: Finite32::new(0.25).expect("finite"),
            gpu_ms: None,
        };
        let value = serde_json::to_value(&unit).expect("serializes");
        assert_eq!(
            value["unit"],
            json!({"type": "item_effects", "scene_id": "main", "item_id": "cam"})
        );
        assert!(value.get("gpu_ms").is_none(), "no GPU time, no field");
        let back: UnitProfile = serde_json::from_value(value).expect("parses");
        assert_eq!(back, unit);
        let newer: ProfileUnit =
            serde_json::from_value(json!({"type": "hologram", "x": 1})).expect("parses");
        assert_eq!(newer, ProfileUnit::Unknown);
        let drawer: Drawer = serde_json::from_value(json!("encoder")).expect("parses");
        assert_eq!(drawer, Drawer::Unknown);
    }

    /// An empty object parses, so every field can be left out by an older sender.
    #[test]
    fn a_profile_with_missing_fields_parses() {
        let profile: ProfileInfo = serde_json::from_value(json!({"future": 1})).expect("parses");
        assert_eq!(profile, ProfileInfo::default());
    }
}
