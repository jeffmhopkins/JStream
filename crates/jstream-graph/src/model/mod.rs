//! The collection state: sources, scenes and the items that place sources in scenes, the
//! outputs, and the encoder profiles they use.
//!
//! Docs: docs/ARCHITECTURE.md#data-model
//!
//! Everything here is read-only from outside the crate. The only way to change it is
//! [`Graph::apply`](crate::Graph::apply), which keeps the invariants below:
//!
//! - Source, scene, item and output IDs are non-empty and unique within their kind. Item
//!   IDs are unique across all scenes.
//! - Every item's source exists, and every `scene` source's scene exists.
//! - Every `builtin` source's kind is one the graph's kinds describe, and its settings
//!   hold exactly that kind's parameters, each valid for its descriptor. A `scene`
//!   source has no settings.
//! - Every scene and source that a settings value refers to exists.
//! - The dependency edges (items placing sources, `scene` sources showing scenes, and
//!   scene and source references in settings) have no cycle.
//! - Every RTMP output's URL is a valid `rtmp://` or `rtmps://` server URL that carries
//!   no credential. Stream keys are never here (R-SEC-001): they live in the OS
//!   credential store, outside the collection.
//! - Every output's encoder profile exists, and every profile holds an encoding the
//!   engine can open: H.264 and AAC, with bit rates and keyframe interval in range.
//!
//! The structs have private fields so that later additions (source `filters[]`, item
//! `effects[]`, the full 3D transform) don't break callers.

use jstream_protocol::{
    Crop, EncoderProfile, EncoderProfileId, ItemId, ItemTransform, KindDescriptor, OutputId,
    OutputKind, Rev, SceneId, Settings, SourceId, SourceKind,
};
use std::collections::BTreeMap;

/// The scene graph of one collection, its revision, and the kinds it can create.
///
/// Starts empty at rev 0. Each successful [`Graph::apply`](crate::Graph::apply) changes
/// the [`State`] and increments the revision by exactly one. The kinds are not
/// collection state: the engine supplies them, and `builtin` sources are validated
/// against them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub(crate) rev: Rev,
    pub(crate) state: State,
    pub(crate) kinds: BTreeMap<String, KindDescriptor>,
}

impl Graph {
    /// An empty graph at rev 0 that describes no kinds, so it can hold only `scene`
    /// sources until [`Graph::with_kinds`] adds some.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds kind descriptors, replacing any with the same ID.
    #[must_use]
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = KindDescriptor>) -> Self {
        self.kinds
            .extend(kinds.into_iter().map(|k| (k.kind_id.clone(), k)));
        self
    }

    /// Every kind this graph can create sources of, ordered by ID.
    pub fn kinds(&self) -> impl ExactSizeIterator<Item = &KindDescriptor> {
        self.kinds.values()
    }

    /// The kind with this ID.
    pub fn kind(&self, id: &str) -> Option<&KindDescriptor> {
        self.kinds.get(id)
    }

    /// The current revision.
    pub fn rev(&self) -> Rev {
        self.rev
    }

    /// The current state.
    pub fn state(&self) -> &State {
        &self.state
    }
}

/// The collection state without its revision: what undo restores exactly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub(crate) sources: BTreeMap<SourceId, Source>,
    pub(crate) scenes: Vec<Scene>,
    pub(crate) outputs: BTreeMap<OutputId, Output>,
    pub(crate) encoders: BTreeMap<EncoderProfileId, Encoder>,
}

impl State {
    /// Every source, ordered by ID. Sources have no display order of their own.
    pub fn sources(&self) -> impl ExactSizeIterator<Item = &Source> {
        self.sources.values()
    }

    /// The source with this ID.
    pub fn source(&self, id: &SourceId) -> Option<&Source> {
        self.sources.get(id)
    }

    /// Every scene, in display order.
    pub fn scenes(&self) -> &[Scene] {
        &self.scenes
    }

    /// The scene with this ID.
    pub fn scene(&self, id: &SceneId) -> Option<&Scene> {
        self.scenes.iter().find(|s| &s.id == id)
    }

    /// The item with this ID and the scene that holds it.
    pub fn item(&self, id: &ItemId) -> Option<(&Scene, &Item)> {
        self.scenes
            .iter()
            .find_map(|s| s.items.iter().find(|i| &i.id == id).map(|i| (s, i)))
    }

    /// The items that place this source, in every scene.
    pub fn placements<'a>(&'a self, source: &'a SourceId) -> impl Iterator<Item = &'a Item> {
        self.scenes
            .iter()
            .flat_map(|s| s.items.iter())
            .filter(move |i| &i.source_id == source)
    }

    /// Every output, ordered by ID. Outputs have no display order of their own.
    pub fn outputs(&self) -> impl ExactSizeIterator<Item = &Output> {
        self.outputs.values()
    }

    /// The output with this ID.
    pub fn output(&self, id: &OutputId) -> Option<&Output> {
        self.outputs.get(id)
    }

    /// Every encoder profile, ordered by ID.
    pub fn encoders(&self) -> impl ExactSizeIterator<Item = &Encoder> {
        self.encoders.values()
    }

    /// The encoder profile with this ID.
    pub fn encoder(&self, id: &EncoderProfileId) -> Option<&Encoder> {
        self.encoders.get(id)
    }

    /// The encoding `output` uses: its profile's, or the engine's default when it has
    /// none. `None` for an output not in the collection.
    pub fn encoding_of(&self, output: &OutputId) -> Option<EncoderProfile> {
        let output = self.outputs.get(output)?;
        Some(match &output.encoder_profile {
            Some(id) => self.encoders.get(id)?.profile,
            None => EncoderProfile::DEFAULT,
        })
    }

    pub(crate) fn scene_index(&self, id: &SceneId) -> Option<usize> {
        self.scenes.iter().position(|s| &s.id == id)
    }

    /// The (scene, item) indexes of an item.
    pub(crate) fn item_index(&self, id: &ItemId) -> Option<(usize, usize)> {
        self.scenes
            .iter()
            .enumerate()
            .find_map(|(si, s)| s.items.iter().position(|i| &i.id == id).map(|ii| (si, ii)))
    }
}

/// One source instance. Every item that places it shows this same instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub(crate) id: SourceId,
    pub(crate) name: String,
    pub(crate) kind: SourceKind,
    pub(crate) settings: Settings,
}

impl Source {
    /// Its ID.
    pub fn id(&self) -> &SourceId {
        &self.id
    }

    /// Its display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What it shows.
    pub fn kind(&self) -> &SourceKind {
        &self.kind
    }

    /// Its settings: every parameter of its kind, or empty for a `scene` source.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}

/// A scene: a stack of items.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub(crate) id: SceneId,
    pub(crate) name: String,
    pub(crate) items: Vec<Item>,
}

impl Scene {
    /// Its ID.
    pub fn id(&self) -> &SceneId {
        &self.id
    }

    /// Its display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its items in stacking order: the first is at the bottom.
    pub fn items(&self) -> &[Item] {
        &self.items
    }
}

/// One placement of a source in a scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub(crate) id: ItemId,
    pub(crate) source_id: SourceId,
    pub(crate) transform: ItemTransform,
    pub(crate) crop: Crop,
    pub(crate) visible: bool,
}

impl Item {
    /// Its ID.
    pub fn id(&self) -> &ItemId {
        &self.id
    }

    /// The source it places.
    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// Where it sits in its scene.
    pub fn transform(&self) -> &ItemTransform {
        &self.transform
    }

    /// How much of the source it cuts off.
    pub fn crop(&self) -> &Crop {
        &self.crop
    }

    /// Whether it is drawn.
    pub fn visible(&self) -> bool {
        self.visible
    }
}

/// One output's configuration: what it is and where it sends. Whether it runs is runtime
/// state, kept by the engine, and its stream key is in the OS credential store.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub(crate) id: OutputId,
    pub(crate) kind: OutputKind,
    pub(crate) encoder_profile: Option<EncoderProfileId>,
}

impl Output {
    /// Its ID.
    pub fn id(&self) -> &OutputId {
        &self.id
    }

    /// What it is and where it sends.
    pub fn kind(&self) -> &OutputKind {
        &self.kind
    }

    /// The encoder profile it uses, or `None` for the engine's default.
    pub fn encoder_profile(&self) -> Option<&EncoderProfileId> {
        self.encoder_profile.as_ref()
    }
}

/// One encoder profile: a named encoding outputs can share.
#[derive(Clone, Debug, PartialEq)]
pub struct Encoder {
    pub(crate) id: EncoderProfileId,
    pub(crate) name: String,
    pub(crate) profile: EncoderProfile,
}

impl Encoder {
    /// Its ID.
    pub fn id(&self) -> &EncoderProfileId {
        &self.id
    }

    /// Its display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The encoding.
    pub fn profile(&self) -> &EncoderProfile {
        &self.profile
    }
}
