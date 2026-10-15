//! Queries: reads with no effect on state, and their results.

use crate::command::{Crop, ItemTransform, SourceKind};
use crate::event::OutputState;
use crate::id::{ItemId, OutputId, Rev, SceneId, SourceId};
use crate::kind::{KindDescriptor, KindSummary};
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
    /// Reports an output's current state.
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
    /// Reads the whole collection state (every source, and every scene with its items)
    /// at one revision: what a client reads on connecting, after a `state.changed` it
    /// didn't cause, or before an edit it sends with `expected_rev`.
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
        /// Every source, ordered by ID.
        sources: Vec<SourceInfo>,
        /// Every scene with its items, in display order.
        scenes: Vec<SceneInfo>,
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
    /// How many items it has.
    pub item_count: u32,
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
}

/// One scene with its items, as `state.get` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneInfo {
    /// The scene's ID.
    pub scene_id: SceneId,
    /// Its display name.
    pub name: String,
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
    /// Where it sits in its scene.
    pub transform: ItemTransform,
    /// How much of the source it cuts off.
    pub crop: Crop,
    /// Whether it is drawn.
    pub visible: bool,
}
