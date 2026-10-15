//! Queries: reads with no effect on state, and their results.

use crate::audio::{BusInfo, SendInfo, SourceAudio};
use crate::command::{Crop, Edit, ItemTransform, SceneRole, SourceKind};
use crate::encoder::{EncoderProfileInfo, EncoderSessionInfo};
use crate::event::OutputState;
use crate::geometry::{Canvas, ItemGeometry, PlacementForm};
use crate::id::{FolderId, ItemId, LabelId, OutputId, Rev, SceneId, SourceId};
use crate::kind::{KindDescriptor, KindSummary};
use crate::organise::{FolderInfo, LabelInfo, SceneMeta, SourceMeta};
use crate::output::OutputInfo;
use crate::settings::Settings;
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
    /// Lists one scene's items, in stacking order (the first is at the bottom). Fails with
    /// `not_found` if the scene doesn't exist.
    #[serde(rename = "item.list")]
    ItemList {
        /// The scene whose items to list.
        scene_id: SceneId,
    },
    /// Reads the whole collection state (the canvas, every source, every scene with its
    /// items, every output, every encoder profile, the buses and sends, and the folders
    /// and labels) at one revision: what a client reads on connecting, after a
    /// `state.changed` it didn't cause, or before an edit it sends with `expected_rev`.
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
    /// Reads the studio state: which scene is on program. It is runtime state, not
    /// collection state (ADR-0030), so the result carries no `rev`.
    #[serde(rename = "studio.get")]
    StudioGet,
    /// Lists the collection's encoder profiles, ordered by ID.
    #[serde(rename = "encoder.list")]
    EncoderList,
    /// Lists the collection's audio buses, ordered by ID, and every send into them.
    #[serde(rename = "bus.list")]
    BusList,
    /// The batch that deletes a scene, source, folder or label together with everything
    /// that refers to it, which `scene.delete`, `source.delete`, `folder.delete` and
    /// `label.delete` alone refuse (their `invalid_argument` lists those `blockers`). The
    /// plan removes the items that place a deleted source, deletes the `scene` sources
    /// that show a deleted scene (and their items), and sets settings references to the
    /// target to `null`, keeping the sources that hold them. For a folder it moves what
    /// sits in it up to the folder's parent; for a label it takes the label off the
    /// sources carrying it.
    /// The client shows it, then sends its `edits` as an ordinary batch with
    /// `expected_rev` set to the result's `rev`; undo is that batch's inverse. Fails with
    /// `not_found` if the target doesn't exist.
    #[serde(rename = "delete.plan")]
    DeletePlan {
        /// What to delete.
        target: DeleteTarget,
    },
}

/// What a `delete.plan` deletes: a scene, a source, a folder or a label.
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
            Query::ItemList { .. } => "item.list",
            Query::StateGet => "state.get",
            Query::VideoSnapshot => "video.snapshot",
            Query::KindList => "kind.list",
            Query::KindDescribe { .. } => "kind.describe",
            Query::OutputList => "output.list",
            Query::StudioGet => "studio.get",
            Query::EncoderList => "encoder.list",
            Query::BusList => "bus.list",
            Query::DeletePlan { .. } => "delete.plan",
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
        /// Every audio bus, ordered by ID, the master among them. Missing (from a
        /// server older than 0.19) parses as empty.
        #[serde(default)]
        buses: Vec<BusInfo>,
        /// Every send, ordered by bus, then by source. Missing (from a server older
        /// than 0.19) parses as empty.
        #[serde(default)]
        sends: Vec<SendInfo>,
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
    },
    /// The result of `encoder.list`.
    #[serde(rename = "encoder.list")]
    EncoderList {
        /// The revision the list was read at.
        rev: Rev,
        /// The profiles, ordered by ID.
        encoders: Vec<EncoderProfileInfo>,
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
        /// `source.set_settings` setting each reference to a deleted object to `null`,
        /// `source.delete` for the `scene` sources that show a deleted scene, and last the
        /// delete of the target itself.
        edits: Vec<Edit>,
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
            QueryResult::EncoderList { .. } => "encoder.list",
            QueryResult::BusList { .. } => "bus.list",
            QueryResult::DeletePlan { .. } => "delete.plan",
        }
    }
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
    /// The folder it is filed in (ADR-0024 §3). Omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderId>,
    /// Its organisation metadata, each field omitted while it has its default.
    #[serde(flatten)]
    pub meta: SceneMeta,
    /// Its items, in stacking order: the first is at the bottom.
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
}
