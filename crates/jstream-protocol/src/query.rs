//! Queries: reads with no effect on state, and their results.

use crate::audio::{BusInfo, SendInfo, SourceAudio};
use crate::command::{BlendMode, Crop, Edit, ItemTransform, SceneRole, SourceKind};
use crate::encoder::{EncoderProfileInfo, EncoderSessionInfo};
use crate::event::OutputState;
use crate::filter::{FilterChains, FilterInfo};
use crate::finite::Finite32;
use crate::geometry::{Canvas, ItemGeometry, PlacementForm};
use crate::hotkey::HotkeyBinding;
use crate::id::{
    BusId, DeviceId, FolderId, ItemId, LabelId, OutputId, Rev, SceneId, SourceId,
    TransitionPresetId,
};
use crate::keyframe::ItemOverride;
use crate::kind::{KindDescriptor, KindSummary};
use crate::live::GestureInfo;
use crate::mix::VideoMixInfo;
use crate::organise::{FolderInfo, LabelInfo, SceneMeta, SourceMeta};
use crate::output::OutputInfo;
use crate::settings::Settings;
use crate::transition::{ItemTransition, TransitionsInfo};
use crate::youtube::{
    AudienceStatsInfo, BroadcastInfo, BroadcastListStatus, BroadcastType, QuotaInfo, StreamInfo,
    YoutubeAuth,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A read. Queries change nothing, so they need no classification as edit or action.
///
/// Serialized with an internal `type` tag holding the query name (`noun.verb`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Query {
    /// Lists the collection's scenes, in display order.
    #[serde(rename = "scene.list")]
    SceneList,
    /// Reports an output's current state, and whether its stream key is set. Fails with
    /// `not_found` for an output the engine doesn't know.
    #[serde(rename = "output.status")]
    OutputStatus {
        /// The output to report on.
        output_id: OutputId,
    },
    /// Lists the collection's sources, ordered by ID. Sources have no display order of
    /// their own.
    #[serde(rename = "source.list")]
    SourceList,
    /// Every source's current size (ADR-0022 §4, 0.40): runtime state, so the result
    /// carries no `rev`. A source drawn at a density reports its own size, the size its
    /// frames are placed at, not its pixels (ADR-0073). `source.size_changed` reports each
    /// change after this.
    #[serde(rename = "source.sizes")]
    SourceSizes,
    /// Lists one scene's items, in stacking order (the first is at the bottom). Fails with
    /// `not_found` if the scene doesn't exist.
    #[serde(rename = "item.list")]
    ItemList {
        /// The scene whose items to list.
        scene_id: SceneId,
    },
    /// Reads the whole collection state (the canvas, every source, every scene with its
    /// items, every output, every encoder profile, every video mix, the buses and sends,
    /// every hotkey binding, and the folders and labels) at one revision: what a client
    /// reads on connecting, after a `state.changed` it didn't cause, or before an edit it
    /// sends with `expected_rev`.
    #[serde(rename = "state.get")]
    StateGet,
    /// The current program mix, as a PNG.
    ///
    /// The result's `png` is standard base64 with padding and no line breaks. `width`
    /// and `height` are that image's size in pixels; the longest side is at most 1280.
    /// `rev` is the collection revision of the layout the image shows. Source pixels are
    /// runtime state and can change without a new `rev`.
    #[serde(rename = "video.snapshot")]
    VideoSnapshot,
    /// Lists every kind the engine can create, ordered by ID. Kinds are not collection
    /// state, so the result carries no `rev`.
    #[serde(rename = "kind.list")]
    KindList,
    /// One kind's full descriptor, and a JSON Schema of its settings generated from it.
    /// Fails with `invalid_argument` on `kind_id` if the engine has no such kind.
    #[serde(rename = "kind.describe")]
    KindDescribe {
        /// The kind to describe, as `kind.list` reports it.
        kind_id: String,
    },
    /// Lists the collection's outputs, ordered by ID, with what each is and where it
    /// sends. Outputs have no display order of their own.
    #[serde(rename = "output.list")]
    OutputList,
    /// Reads the studio state: which scenes are on program and on preview. It is runtime
    /// state, not collection state (ADR-0030), so the result carries no `rev`.
    #[serde(rename = "studio.get")]
    StudioGet,
    /// **Deprecated** (ADR-0080), removed in 1.0. JStream has no privacy
    /// screen, so the answer is always `on: false`.
    #[serde(rename = "privacy.get")]
    PrivacyGet,
    /// Lists the collection's encoder profiles, ordered by ID.
    #[serde(rename = "encoder.list")]
    EncoderList,
    /// Lists the collection's video mixes, ordered by ID.
    #[serde(rename = "video_mix.list")]
    VideoMixList,
    /// Lists the open live-value gestures (ADR-0032), every client's, ordered by client
    /// and gesture ID, with the values each holds now: what a client that connects
    /// during a drag reads to show it. Live values are runtime state, not collection
    /// state, so the result carries no `rev`.
    #[serde(rename = "live.list")]
    LiveList,
    /// Whether the engine is signed in to YouTube. Never calls YouTube.
    #[serde(rename = "youtube.status")]
    YoutubeStatus,
    /// Lists the signed-in channel's broadcasts, one page of up to 50 at a time, in
    /// YouTube's order. Calls the Live API, so it fails with `action_failed` when not
    /// signed in or YouTube refuses (including when the day's quota is used up).
    #[serde(rename = "youtube.list_broadcasts")]
    YoutubeListBroadcasts {
        /// Which broadcasts, by state. Omitted means `all`.
        #[serde(default)]
        status: BroadcastListStatus,
        /// Which broadcasts, by type. Omitted means `all`, which includes the channel's
        /// persistent ("Stream now") broadcast.
        #[serde(default)]
        broadcast_type: BroadcastType,
        /// The `next_page_token` of an earlier page, for the page after it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_token: Option<String>,
    },
    /// Lists the signed-in channel's streams, one page of up to 50 at a time. Their keys
    /// are never reported. Fails like `youtube.list_broadcasts`.
    #[serde(rename = "youtube.list_streams")]
    YoutubeListStreams {
        /// The `next_page_token` of an earlier page, for the page after it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_token: Option<String>,
    },
    /// The audience of each running `youtube` output, as last read, and the engine's
    /// estimate of the day's API quota (ADR-0041 §3 and §5, 0.38). Never calls YouTube.
    #[serde(rename = "youtube.stats")]
    YoutubeStats,
    /// Lists the collection's audio buses, ordered by ID, and every send into them.
    #[serde(rename = "bus.list")]
    BusList,
    /// The batch that deletes a scene, source, bus, folder or label together with
    /// everything that refers to it, which `scene.delete`, `source.delete`, `bus.delete`,
    /// `folder.delete` and `label.delete` alone refuse (their `invalid_argument` lists
    /// those `blockers`). The plan removes the items that place a deleted source, deletes
    /// the `scene` sources that show a deleted scene (and their items), and sets settings
    /// references to the target to `null`, keeping the sources that hold them. For a
    /// folder it moves what sits in it up to the folder's parent; for a label it takes
    /// the label off the sources carrying it. It also lists the hotkey bindings the
    /// delete removes with what they name (ADR-0040 §3), which need no edit of their own.
    /// The client shows it, then sends its `edits` as an ordinary batch with
    /// `expected_rev` set to the result's `rev`; undo is that batch's inverse. Fails with
    /// `not_found` if the target doesn't exist.
    #[serde(rename = "delete.plan")]
    DeletePlan {
        /// What to delete.
        target: DeleteTarget,
    },
    /// Lists the displays and windows the engine can capture now: what a `display` or
    /// `window` source's `target` setting can name. They are runtime state, not
    /// collection state, so the result carries no `rev`, and a window closed since may
    /// still be named by a source. Fails with `action_failed` when the engine has no
    /// capture backend, or the OS refuses to list them.
    #[serde(rename = "capture.targets")]
    CaptureTargets,
    /// Lists the paired devices (ADR-0044), ordered by ID: each one's name and when it
    /// paired and was last used, never its token. Answered by the server that admits
    /// clients, not the engine: a `Control` with no server in front fails with
    /// `unsupported`.
    #[serde(rename = "device.list")]
    DeviceList,
}

/// What a `delete.plan` deletes: a scene, a source, (since 0.24) a bus, a folder or a
/// label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeleteTarget {
    /// A scene.
    Scene {
        /// The scene to delete.
        scene_id: SceneId,
    },
    /// A source.
    Source {
        /// The source to delete.
        source_id: SourceId,
    },
    /// A bus other than the master. Since 0.24.
    Bus {
        /// The bus to delete.
        bus_id: BusId,
    },
    /// A folder. The plan moves the scenes and folders in it up to its parent (to the
    /// top level for a top-level folder), then deletes it.
    Folder {
        /// The folder to delete.
        folder_id: FolderId,
    },
    /// A label. The plan takes it off every source that carries it, then deletes it.
    Label {
        /// The label to delete.
        label_id: LabelId,
    },
}

impl Query {
    /// The query name, as used in the `type` tag (`scene.list`).
    pub fn name(&self) -> &'static str {
        match self {
            Query::SceneList => "scene.list",
            Query::OutputStatus { .. } => "output.status",
            Query::SourceList => "source.list",
            Query::SourceSizes => "source.sizes",
            Query::ItemList { .. } => "item.list",
            Query::StateGet => "state.get",
            Query::VideoSnapshot => "video.snapshot",
            Query::KindList => "kind.list",
            Query::KindDescribe { .. } => "kind.describe",
            Query::OutputList => "output.list",
            Query::StudioGet => "studio.get",
            Query::PrivacyGet => "privacy.get",
            Query::EncoderList => "encoder.list",
            Query::VideoMixList => "video_mix.list",
            Query::LiveList => "live.list",
            Query::YoutubeStatus => "youtube.status",
            Query::YoutubeListBroadcasts { .. } => "youtube.list_broadcasts",
            Query::YoutubeListStreams { .. } => "youtube.list_streams",
            Query::YoutubeStats => "youtube.stats",
            Query::BusList => "bus.list",
            Query::DeletePlan { .. } => "delete.plan",
            Query::CaptureTargets => "capture.targets",
            Query::DeviceList => "device.list",
        }
    }
}

/// The result of a [`Query`]. Its `type` tag repeats the query's name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum QueryResult {
    /// The result of `scene.list`.
    #[serde(rename = "scene.list")]
    SceneList {
        /// The revision the list was read at.
        rev: Rev,
        /// The scenes, in display order.
        scenes: Vec<SceneSummary>,
    },
    /// The result of `output.status`. Output state is runtime state, not collection
    /// state, so it carries no `rev`.
    #[serde(rename = "output.status")]
    OutputStatus {
        /// The output reported on.
        output_id: OutputId,
        /// Its current state.
        state: OutputState,
        /// Whether the credential store holds a stream key for it. The key itself is
        /// never reported. Missing (from a server older than 0.7) parses as `false`.
        #[serde(default)]
        key_set: bool,
        /// The encoder session the output uses while it runs: which profile, and which
        /// other running outputs share it. Absent while it isn't running, and from a
        /// server older than 0.13.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoder: Option<EncoderSessionInfo>,
        /// How many video encoder sessions the engine has open now, for every output,
        /// and how many of them are hardware ones. Hardware encoders cap their
        /// concurrent sessions per GPU vendor and driver, and the engine doesn't guess
        /// the cap. Missing (from a server older than 0.13) parses as 0.
        #[serde(default)]
        encoder_sessions: u32,
        /// Of `encoder_sessions`, the hardware ones. Missing parses as 0.
        #[serde(default)]
        hardware_sessions: u32,
        /// Whether publishing sends the stream key across the network unencrypted:
        /// a generic RTMP target whose server URL is plain `rtmp://` to a host other than
        /// the engine's machine (loopback and `localhost` don't count). A client shows it
        /// as a warning; the engine still publishes. Missing (from a server older than
        /// 0.37) parses as `false`.
        #[serde(default)]
        cleartext: bool,
    },
    /// The result of `source.list`.
    #[serde(rename = "source.list")]
    SourceList {
        /// The revision the list was read at.
        rev: Rev,
        /// The sources, ordered by ID.
        sources: Vec<SourceInfo>,
    },
    /// The result of `item.list`.
    #[serde(rename = "item.list")]
    ItemList {
        /// The revision the list was read at.
        rev: Rev,
        /// The scene listed.
        scene_id: SceneId,
        /// Its items, in stacking order: the first is at the bottom.
        items: Vec<ItemInfo>,
    },
    /// The result of `state.get`: the whole collection state at one revision.
    #[serde(rename = "state.get")]
    StateGet {
        /// The revision the state was read at.
        rev: Rev,
        /// The canvas every scene is laid out on. Missing (from a server older than 0.11)
        /// parses as 1920×1080.
        #[serde(default)]
        canvas: Canvas,
        /// Every source, ordered by ID.
        sources: Vec<SourceInfo>,
        /// Every scene with its items, in display order.
        scenes: Vec<SceneInfo>,
        /// Every output, ordered by ID. Missing (from a server older than 0.7) parses
        /// as empty.
        #[serde(default)]
        outputs: Vec<OutputInfo>,
        /// Every encoder profile, ordered by ID. Missing (from a server older than
        /// 0.13) parses as empty.
        #[serde(default)]
        encoders: Vec<EncoderProfileInfo>,
        /// Every video mix, ordered by ID. Missing (from a server older than 0.28)
        /// parses as empty.
        #[serde(default)]
        video_mixes: Vec<VideoMixInfo>,
        /// Every audio bus, ordered by ID, the master among them. Missing (from a
        /// server older than 0.19) parses as empty.
        #[serde(default)]
        buses: Vec<BusInfo>,
        /// Every send, ordered by bus, then by source. Missing (from a server older
        /// than 0.19) parses as empty.
        #[serde(default)]
        sends: Vec<SendInfo>,
        /// The master stage. Omitted while it has no filters, so missing (also from a
        /// server older than 0.29) parses as empty.
        #[serde(default, skip_serializing_if = "MasterInfo::is_empty")]
        master: Box<MasterInfo>,
        /// Every hotkey binding, ordered by chord. Missing (from a server older than
        /// 0.25) parses as empty.
        #[serde(default)]
        hotkeys: Vec<HotkeyBinding>,
        /// The transition presets and which one is the default (ADR-0029). Missing
        /// (from a server older than 0.30) parses as the one `cut` preset every
        /// collection starts with.
        #[serde(default)]
        transitions: Box<TransitionsInfo>,
        /// Every folder of the scene list, in folder order (ADR-0024 §3). Omitted when
        /// there are none; missing (from an older server) parses as empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        folders: Vec<FolderInfo>,
        /// Every label, in label order (ADR-0024 §4). Omitted when there are none;
        /// missing (from an older server) parses as empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<LabelInfo>,
    },
    /// The result of `video.snapshot`: the program mix encoded as a PNG.
    #[serde(rename = "video.snapshot")]
    VideoSnapshot {
        /// The collection revision of the layout this image shows.
        rev: Rev,
        /// PNG width in pixels.
        width: u32,
        /// PNG height in pixels.
        height: u32,
        /// The PNG file, standard base64 with padding and no line breaks.
        png: String,
    },
    /// The result of `kind.list`.
    #[serde(rename = "kind.list")]
    KindList {
        /// Every kind, ordered by ID.
        kinds: Vec<KindSummary>,
    },
    /// The result of `kind.describe`.
    #[serde(rename = "kind.describe")]
    KindDescribe {
        /// The kind's descriptor.
        kind: KindDescriptor,
        /// A JSON Schema (draft 2020-12) of a complete settings map of this kind,
        /// generated from `kind`.
        settings_schema: serde_json::Value,
    },
    /// The result of `output.list`.
    #[serde(rename = "output.list")]
    OutputList {
        /// The revision the list was read at.
        rev: Rev,
        /// The outputs, ordered by ID.
        outputs: Vec<OutputInfo>,
    },
    /// The result of `studio.get`. Studio state is runtime state, so it carries no `rev`.
    #[serde(rename = "studio.get")]
    StudioGet {
        /// The scene on program. Omitted only while the collection has no scene.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        program_scene: Option<SceneId>,
        /// The scene on preview, which `studio.set_preview` sets. Omitted only while the
        /// collection has no scene, and by a server older than 0.30.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview_scene: Option<SceneId>,
    },
    /// The result of the deprecated `privacy.get` (ADR-0080): always `on: false`.
    #[serde(rename = "privacy.get")]
    PrivacyGet {
        /// Whether the privacy screen covers the outputs.
        on: bool,
    },
    /// The result of `encoder.list`.
    #[serde(rename = "encoder.list")]
    EncoderList {
        /// The revision the list was read at.
        rev: Rev,
        /// The profiles, ordered by ID.
        encoders: Vec<EncoderProfileInfo>,
    },
    /// The result of `video_mix.list`.
    #[serde(rename = "video_mix.list")]
    VideoMixList {
        /// The revision the list was read at.
        rev: Rev,
        /// The mixes, ordered by ID.
        video_mixes: Vec<VideoMixInfo>,
    },
    /// The result of `live.list`.
    #[serde(rename = "live.list")]
    LiveList {
        /// The open gestures, ordered by client ID, then gesture ID.
        gestures: Vec<GestureInfo>,
    },
    /// The result of `youtube.status`. Runtime state, so it carries no `rev`.
    #[serde(rename = "youtube.status")]
    YoutubeStatus {
        /// Whether the engine is signed in.
        auth: YoutubeAuth,
    },
    /// The result of `youtube.list_broadcasts`.
    #[serde(rename = "youtube.list_broadcasts")]
    YoutubeListBroadcasts {
        /// This page's broadcasts.
        broadcasts: Vec<BroadcastInfo>,
        /// The token for the next page, if there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_page_token: Option<String>,
    },
    /// The result of `youtube.list_streams`.
    #[serde(rename = "youtube.list_streams")]
    YoutubeListStreams {
        /// This page's streams.
        streams: Vec<StreamInfo>,
        /// The token for the next page, if there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_page_token: Option<String>,
    },
    /// The result of `youtube.stats`. Runtime state, so it carries no `rev`.
    #[serde(rename = "youtube.stats")]
    YoutubeStats {
        /// The audience of each running `youtube` output YouTube has reported one for,
        /// ordered by output ID.
        outputs: Vec<AudienceStatsInfo>,
        /// The day's estimated quota.
        quota: QuotaInfo,
    },
    /// The result of `bus.list`.
    #[serde(rename = "bus.list")]
    BusList {
        /// The revision the list was read at.
        rev: Rev,
        /// The buses, ordered by ID, the master among them.
        buses: Vec<BusInfo>,
        /// Every send, ordered by bus, then by source.
        sends: Vec<SendInfo>,
    },
    /// The result of `delete.plan`.
    #[serde(rename = "delete.plan")]
    DeletePlan {
        /// The revision the plan was made at. Send `edits` with this `expected_rev`, so
        /// the batch is refused if the state changed since.
        rev: Rev,
        /// The edits, in order: `item.remove` for the items that place a deleted source,
        /// `source.set_settings` and `filter.set_settings` setting each reference to a
        /// deleted object to `null`, `source.delete` for the `scene` sources that show a deleted scene, and last the
        /// delete of the target itself.
        edits: Vec<Edit>,
        /// The hotkey bindings the batch removes because their action names an object it
        /// deletes, ordered by chord, so the client can say "this also removes the hotkey
        /// Ctrl+Digit1". The deletes remove them; undo restores them. Omitted when there
        /// are none, and missing (from a server older than 0.25) parses as empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hotkeys: Vec<HotkeyBinding>,
    },
    /// The result of `source.sizes`. Runtime state, so it carries no `rev`.
    #[serde(rename = "source.sizes")]
    SourceSizes {
        /// Every source in the collection, ordered by ID.
        sources: Vec<SourceSize>,
    },
    /// The result of `capture.targets`.
    #[serde(rename = "capture.targets")]
    CaptureTargets {
        /// The displays, then the windows, in the order the OS lists them.
        targets: Vec<CaptureTargetInfo>,
    },
    /// The result of `device.list`. Devices are not collection state, so it carries no
    /// `rev`.
    #[serde(rename = "device.list")]
    DeviceList {
        /// The paired devices, ordered by ID.
        devices: Vec<DeviceInfo>,
    },
}

impl QueryResult {
    /// The name of the query this answers.
    pub fn name(&self) -> &'static str {
        match self {
            QueryResult::SceneList { .. } => "scene.list",
            QueryResult::OutputStatus { .. } => "output.status",
            QueryResult::SourceList { .. } => "source.list",
            QueryResult::ItemList { .. } => "item.list",
            QueryResult::StateGet { .. } => "state.get",
            QueryResult::VideoSnapshot { .. } => "video.snapshot",
            QueryResult::KindList { .. } => "kind.list",
            QueryResult::KindDescribe { .. } => "kind.describe",
            QueryResult::OutputList { .. } => "output.list",
            QueryResult::StudioGet { .. } => "studio.get",
            QueryResult::PrivacyGet { .. } => "privacy.get",
            QueryResult::EncoderList { .. } => "encoder.list",
            QueryResult::VideoMixList { .. } => "video_mix.list",
            QueryResult::LiveList { .. } => "live.list",
            QueryResult::YoutubeStatus { .. } => "youtube.status",
            QueryResult::YoutubeListBroadcasts { .. } => "youtube.list_broadcasts",
            QueryResult::YoutubeListStreams { .. } => "youtube.list_streams",
            QueryResult::YoutubeStats { .. } => "youtube.stats",
            QueryResult::BusList { .. } => "bus.list",
            QueryResult::DeletePlan { .. } => "delete.plan",
            QueryResult::SourceSizes { .. } => "source.sizes",
            QueryResult::CaptureTargets { .. } => "capture.targets",
            QueryResult::DeviceList { .. } => "device.list",
        }
    }
}

/// One paired device, as `device.list` reports it. Its token is never reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceInfo {
    /// The device's ID, which `device.revoke` takes.
    pub device_id: DeviceId,
    /// The name it gave when it paired. The audit log names its sessions
    /// `device:<name>`.
    pub name: String,
    /// When it paired, in seconds since the Unix epoch (UTC).
    pub paired_at: u64,
    /// When its token was last presented, in seconds since the Unix epoch (UTC), to the
    /// hour. A token unused for 30 days lapses and the device is dropped.
    pub last_used_at: u64,
}

/// One display or window in a `capture.targets` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaptureTargetInfo {
    /// Whether it is a display or a window: the kind of source that captures it.
    pub kind: CaptureTargetKind,
    /// Its capture target ID, the value of that source's `target` setting.
    pub target: String,
    /// A name to show: the display's name or the window's title.
    pub name: String,
    /// Its size in pixels now, when the OS reports it without capturing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<PixelSize>,
    /// Whether it is the primary display, which a `display` source with an empty
    /// `target` captures. Omitted when `false`, and always `false` for a window.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub primary: bool,
}

/// What a capture target is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CaptureTargetKind {
    /// A whole display, captured by a `display` source.
    Display,
    /// One top-level window, captured by a `window` source.
    Window,
}

/// A size in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PixelSize {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// One source's current size, in a `source.sizes` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceSize {
    /// The source.
    pub source_id: SourceId,
    /// Its current frame's size. Omitted while the source has no video frame: it hasn't
    /// started, it has no video (an audio-only kind), or it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<PixelSize>,
}

/// One scene in a `scene.list` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SceneSummary {
    /// The scene's ID.
    pub scene_id: SceneId,
    /// Its display name.
    pub name: String,
    /// What it is for: a `group` that clients hide from the scene list, or a `scene`.
    /// Omitted for a `scene`.
    #[serde(default, skip_serializing_if = "SceneRole::is_scene")]
    pub role: SceneRole,
    /// How many items it has.
    pub item_count: u32,
    /// The folder it is filed in (ADR-0024 §3). Omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderId>,
    /// Its organisation metadata, each field omitted while it has its default.
    #[serde(flatten)]
    pub meta: SceneMeta,
}

/// The master stage, as `state.get` reports it: what applies to everything leaving
/// JStream (ADR-0080 §1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MasterInfo {
    /// Its chains, which `filter.*` edits with the owner `{"type":"master"}`. Only the
    /// audio chain holds filters so far. Omitted while both are empty.
    #[serde(default, skip_serializing_if = "FilterChains::is_empty")]
    pub filters: FilterChains,
}

impl MasterInfo {
    /// Whether it has nothing to report, as `state.get` then omits it.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

/// One source, as `source.list` and `state.get` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceInfo {
    /// The source's ID.
    pub source_id: SourceId,
    /// Its display name.
    pub name: String,
    /// What it shows.
    pub kind: SourceKind,
    /// Its settings: every parameter of its kind. Empty (and omitted) for a `scene`
    /// source and for a kind without parameters.
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    pub settings: Settings,
    /// The source this one was copied from by `source.duplicate`, while that source
    /// exists. Omitted if it was not copied, or its original has been deleted. An item
    /// placing this source can go back to placing the original with `item.set_source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicated_from: Option<SourceId>,
    /// Its filter chains (ADR-0026). Omitted while both are empty.
    #[serde(default, skip_serializing_if = "FilterChains::is_empty")]
    pub filters: FilterChains,
    /// Its audio settings, for an audio source: one whose kind produces audio, or a
    /// `scene` source. Omitted for every other source, and by a server older than 0.19.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<SourceAudio>,
    /// Its labels and favourite flag (ADR-0024 §4), each omitted while it has its
    /// default.
    #[serde(flatten)]
    pub meta: SourceMeta,
}

/// One scene with its items, as `state.get` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneInfo {
    /// The scene's ID.
    pub scene_id: SceneId,
    /// Its display name.
    pub name: String,
    /// What it is for: a `group` that clients hide from the scene list, or a `scene`.
    /// Omitted for a `scene`.
    #[serde(default, skip_serializing_if = "SceneRole::is_scene")]
    pub role: SceneRole,
    /// The preset a switch into this scene uses, unless the switch names one. Omitted
    /// for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_in: Option<TransitionPresetId>,
    /// The preset a switch out of this scene uses, unless the switch or the next scene
    /// names one. Omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_out: Option<TransitionPresetId>,
    /// The folder it is filed in (ADR-0024 §3). Omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderId>,
    /// Its organisation metadata, each field omitted while it has its default.
    #[serde(flatten)]
    pub meta: SceneMeta,
    /// The scene this one is a keyframe scene of (ADR-0024 §2, protocol 0.39), whose
    /// items it shows. Omitted for a scene that is not a variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_of: Option<SceneId>,
    /// A keyframe scene's overrides of its parent's items, in item ID order. Omitted
    /// when there are none, and for every scene that is not a variant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<ItemOverride>,
    /// Its filter chains (ADR-0065, since 0.43), which `filter.*` edits with the owner
    /// `{"type":"scene","scene_id":…}`. They run on the scene's composite. Only the video
    /// chain holds filters, and a keyframe scene has none of its own, since it shows its
    /// parent's. Omitted while both are empty, so missing (also from an older server)
    /// parses as none.
    #[serde(default, skip_serializing_if = "FilterChains::is_empty")]
    pub filters: FilterChains,
    /// Its items, in stacking order: the first is at the bottom. Always empty for a
    /// keyframe scene, which shows its parent's.
    pub items: Vec<ItemInfo>,
}

/// One item, a placement of a source in a scene, as `item.list` and `state.get` report
/// it. It carries every property an edit can set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ItemInfo {
    /// The item's ID.
    pub item_id: ItemId,
    /// The source it places.
    pub source_id: SourceId,
    /// Which form its placement is stored in. Missing (from a server older than 0.11)
    /// parses as `legacy`.
    #[serde(default)]
    pub placement: PlacementForm,
    /// **Deprecated** since 0.11 (ADR-0022), removed in 1.0: read `geometry`. For a
    /// legacy item, the stored transform exactly as written. For a geometry item, a
    /// view computed from the geometry alone: the box's rotated top-left corner, its
    /// rotation, and scale (1, 1).
    pub transform: ItemTransform,
    /// The stored geometry. Absent for a legacy item, whose box depends on the source's
    /// runtime size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<ItemGeometry>,
    /// How much of the source it cuts off.
    pub crop: Crop,
    /// Whether it is drawn.
    pub visible: bool,
    /// Its alpha multiplier, from 0 to 1. Missing (from a server older than this field)
    /// parses as 1.
    #[serde(default = "opaque")]
    pub opacity: Finite32,
    /// How it composites onto what is below it. Missing parses as `normal`.
    #[serde(default)]
    pub blend: BlendMode,
    /// Its transition settings (ADR-0029 §6). Omitted while they are the defaults it
    /// was added with.
    #[serde(default, skip_serializing_if = "ItemTransition::is_default")]
    pub transition: ItemTransition,
    /// Its effects (ADR-0065, since 0.43): a video chain, first filter first, which
    /// `filter.*` edits with the owner `{"type":"item","item_id":…}`. They run on this
    /// placement's cropped, fitted box, before its opacity and blend. Omitted while
    /// empty, so missing (also from an older server) parses as none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<FilterInfo>,
}

fn opaque() -> Finite32 {
    Finite32::ONE
}
