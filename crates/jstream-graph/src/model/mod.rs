//! The collection state: the canvas, sources, scenes and the items that place sources in
//! scenes, the outputs, and the encoder profiles they use.
//!
//! Docs: docs/ARCHITECTURE.md#data-model, docs/adr/0022-item-transform-and-canvas.md
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
//! - Every scene and source that a settings value refers to exists. A source's
//!   `duplicated_from` is a plain link, not a reference: it never names the source
//!   itself, but may name one that no longer exists.
//! - The dependency edges (items placing sources, `scene` sources showing scenes, and
//!   scene and source references in settings) have no cycle.
//! - The canvas is at least 1×1 and at most [`MAX_CANVAS_EDGE`] on each side.
//! - Every item's placement is a legacy transform or a valid geometry: positive size, a
//!   positive perspective if any, and (until the compositor draws 3D) neutral tilt, turn,
//!   depth and perspective.
//! - Every RTMP output's URL is a valid `rtmp://` or `rtmps://` server URL that carries
//!   no credential. Stream keys are never here (R-SEC-001): they live in the OS
//!   credential store, outside the collection.
//! - Every output's encoder profile exists, and every profile holds an encoding the
//!   engine can open: H.264 and AAC, with bit rates and keyframe interval in range.
//! - There is at least one transition preset, and the default names one of them. Every
//!   preset's kind is a transition kind the graph's kinds describe, with exactly its
//!   parameters, and a duration of at most [`MAX_DURATION_MS`]. Every scene's
//!   `transition_in` and `transition_out` name existing presets, and every item's
//!   transition settings are valid (ADR-0029).
//!
//! The structs have private fields so that later additions (source `filters[]`, item
//! `effects[]`, the full 3D transform) don't break callers.

#[cfg(doc)]
use jstream_protocol::MAX_DURATION_MS;
use jstream_protocol::{
    Canvas, Crop, Easing, EncoderProfile, EncoderProfileId, INITIAL_PRESET, ItemGeometry, ItemId,
    ItemTransform, ItemTransition, KindDescriptor, OutputId, OutputKind, PlacementForm, Rev,
    SceneId, SceneRole, Settings, SourceId, SourceKind, TransitionPresetId, TransitionPresetInfo,
    TransitionsInfo, builtin_transition_kinds, initial_preset,
};
use std::collections::BTreeMap;

/// The scene graph of one collection, its revision, and the kinds it can create.
///
/// Starts empty at rev 0. Each successful [`Graph::apply`](crate::Graph::apply) changes
/// the [`State`] and increments the revision by exactly one. The kinds are not
/// collection state: the engine supplies them, and `builtin` sources and transition
/// presets are validated against them.
#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    pub(crate) rev: Rev,
    pub(crate) state: State,
    pub(crate) kinds: BTreeMap<String, KindDescriptor>,
}

impl Default for Graph {
    fn default() -> Self {
        Graph {
            rev: Rev::default(),
            state: State::default(),
            kinds: builtin_transition_kinds()
                .into_iter()
                .map(|k| (k.kind_id.clone(), k))
                .collect(),
        }
    }
}

impl Graph {
    /// An empty graph at rev 0 that describes only the built-in transition kinds
    /// (`cut`, `fade`, `move`), so it can hold only `scene` sources until
    /// [`Graph::with_kinds`] adds source kinds.
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

    /// Every kind this graph can create sources or presets of, ordered by ID.
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

/// The largest canvas side this engine accepts, in pixels. ADR-0022 asks for at least
/// 7680×4320; each side may be up to this, in either orientation.
pub const MAX_CANVAS_EDGE: u32 = 8192;

/// The collection state without its revision: what undo restores exactly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub(crate) canvas: Canvas,
    pub(crate) sources: BTreeMap<SourceId, Source>,
    pub(crate) scenes: Vec<Scene>,
    pub(crate) outputs: BTreeMap<OutputId, Output>,
    pub(crate) encoders: BTreeMap<EncoderProfileId, Encoder>,
    pub(crate) transitions: Transitions,
}

impl State {
    /// The canvas every scene is laid out on.
    pub fn canvas(&self) -> Canvas {
        self.canvas
    }

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

    /// Every transition preset, ordered by ID.
    pub fn presets(&self) -> impl ExactSizeIterator<Item = &Preset> {
        self.transitions.presets.values()
    }

    /// The transition preset with this ID.
    pub fn preset(&self, id: &TransitionPresetId) -> Option<&Preset> {
        self.transitions.presets.get(id)
    }

    /// The default transition preset's ID. It always names a preset.
    pub fn default_preset(&self) -> &TransitionPresetId {
        &self.transitions.default
    }

    /// The presets and the default, as `state.get` reports them.
    pub fn transitions_info(&self) -> TransitionsInfo {
        TransitionsInfo {
            presets: self.presets().map(Preset::to_info).collect(),
            default: self.transitions.default.clone(),
        }
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
    pub(crate) duplicated_from: Option<SourceId>,
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

    /// The source it was copied from, as `source.duplicate` recorded it (ADR-0023). A
    /// plain link, not a dependency edge: it may name a source that has since been
    /// deleted, and queries report it only while that source exists.
    pub fn duplicated_from(&self) -> Option<&SourceId> {
        self.duplicated_from.as_ref()
    }
}

/// The collection's transition presets and its default (ADR-0029).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Transitions {
    pub(crate) presets: BTreeMap<TransitionPresetId, Preset>,
    pub(crate) default: TransitionPresetId,
}

impl Default for Transitions {
    /// What a new collection starts with: the one `cut` preset, as its default.
    fn default() -> Self {
        let preset = Preset::from_info(initial_preset());
        Transitions {
            presets: [(preset.id.clone(), preset)].into_iter().collect(),
            default: TransitionPresetId::new(INITIAL_PRESET),
        }
    }
}

/// One transition preset: which kind of transition a switch runs, and its timing.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub(crate) id: TransitionPresetId,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) duration_ms: u32,
    pub(crate) easing: Easing,
    pub(crate) settings: Settings,
}

impl Preset {
    /// Its ID.
    pub fn id(&self) -> &TransitionPresetId {
        &self.id
    }

    /// Its display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its transition kind's ID (`cut`, `fade`, `move`).
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// How long one item takes, in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }

    /// The curve its items follow unless they set their own.
    pub fn easing(&self) -> Easing {
        self.easing
    }

    /// Its kind's settings: every parameter of the kind.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// As `state.get` reports it.
    pub fn to_info(&self) -> TransitionPresetInfo {
        TransitionPresetInfo {
            preset_id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            duration_ms: self.duration_ms,
            easing: self.easing,
            settings: self.settings.clone(),
        }
    }

    pub(crate) fn from_info(info: TransitionPresetInfo) -> Self {
        Preset {
            id: info.preset_id,
            name: info.name,
            kind: info.kind,
            duration_ms: info.duration_ms,
            easing: info.easing,
            settings: info.settings,
        }
    }
}

/// A scene: a stack of items. A group is a scene too, with the role `group`.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub(crate) id: SceneId,
    pub(crate) name: String,
    pub(crate) role: SceneRole,
    pub(crate) transition_in: Option<TransitionPresetId>,
    pub(crate) transition_out: Option<TransitionPresetId>,
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

    /// What it is for. The role changes no engine behaviour.
    pub fn role(&self) -> SceneRole {
        self.role
    }

    /// The preset a switch into it uses, unless the switch names one.
    pub fn transition_in(&self) -> Option<&TransitionPresetId> {
        self.transition_in.as_ref()
    }

    /// The preset a switch out of it uses, unless the switch or the next scene names one.
    pub fn transition_out(&self) -> Option<&TransitionPresetId> {
        self.transition_out.as_ref()
    }

    /// Its items in stacking order: the first is at the bottom.
    pub fn items(&self) -> &[Item] {
        &self.items
    }
}

/// Where an item sits, in one of ADR-0022's two forms. Only an edit changes the form.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Placement {
    /// A protocol 0.3 transform exactly as written, evaluated against the source's
    /// current size whenever it is drawn.
    Legacy(ItemTransform),
    /// A box on the canvas, which needs no source size.
    Geometry(ItemGeometry),
}

impl Placement {
    /// Which form this is, as `ItemInfo.placement` reports it.
    pub fn form(&self) -> PlacementForm {
        match self {
            Placement::Legacy(_) => PlacementForm::Legacy,
            Placement::Geometry(_) => PlacementForm::Geometry,
        }
    }
}

/// One placement of a source in a scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub(crate) id: ItemId,
    pub(crate) source_id: SourceId,
    pub(crate) placement: Placement,
    pub(crate) crop: Crop,
    pub(crate) visible: bool,
    pub(crate) transition: ItemTransition,
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

    /// Where it sits in its scene, in its stored form.
    pub fn placement(&self) -> &Placement {
        &self.placement
    }

    /// How much of the source it cuts off.
    pub fn crop(&self) -> &Crop {
        &self.crop
    }

    /// Whether it is drawn.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Its transition settings (ADR-0029 §6).
    pub fn transition(&self) -> &ItemTransition {
        &self.transition
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
