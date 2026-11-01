//! The collection file: the collection saved as versioned JSON, and loaded back.
//!
//! [`Graph::to_collection_json`] writes the state and `rev` as JSON with a
//! `schema_version`. [`Graph::with_collection_json`] reads it back: it upgrades an older
//! file through the forward migrations (R-API-003), then rebuilds the state by applying
//! the edits that create it, so a loaded collection passes exactly the checks an edit
//! does (IDs, references, settings against the kinds' descriptors, cycles, output
//! targets). Nothing here touches the file system; the engine reads and writes the file.
//!
//! What is saved is collection state only: no runtime state (whether an output runs),
//! no undo history, and no secret. Stream keys are in the OS credential store and output
//! URLs never carry one (R-SEC-001).
//!
//! Docs: docs/ARCHITECTURE.md#persistence, docs/RULES.md#R-API-003, crates/jstream-graph/README.md#purpose

use crate::model::MASTER_NAME;
use crate::{Graph, Placement, audio};
use jstream_protocol::{
    Action, BlendMode, BusId, BusMonitor, Canvas, Chord, Crop, Easing, Edit, EncoderProfile,
    EncoderProfileId, FilterChain, FilterId, FilterOwner, Finite32, FolderId, INITIAL_PRESET,
    ItemGeometry, ItemId, ItemOverride, ItemTransform, ItemTransition, LabelId, OutputId,
    OutputKind, ProtocolError, Rev, SceneId, SceneMeta, SceneOptions, SceneRole, Settings,
    SourceAudio, SourceId, SourceKind, SourceMeta, TransitionPresetId, VideoMix, VideoMixId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

/// The collection file's current `schema_version`. Bumping it ships a migration in the
/// crate's private `MIGRATIONS` table and a fixture of the previous version that a test
/// loads (R-API-003).
pub const SCHEMA_VERSION: u32 = 2;

/// The built-in `webcam` kind, which gained audio (its device's paired input) after
/// collection files existed; see `rebuild_audio`.
const WEBCAM: &str = "webcam";

/// Whether `kind` is the built-in `webcam`.
fn is_webcam(kind: &SourceKind) -> bool {
    matches!(kind, SourceKind::Builtin { kind } if kind == WEBCAM)
}

/// A migration upgrades a file's JSON from one schema version to the next, in place.
type Migration = fn(&mut Value) -> Result<(), String>;

/// `MIGRATIONS[n]` upgrades version `n + 1` to `n + 2`, so a file at version `v` runs
/// `MIGRATIONS[v - 1..]`.
const MIGRATIONS: [Migration; SCHEMA_VERSION as usize - 1] = [v1_to_v2];

/// Version 2 adds scene filters (`scenes[].filters`) and item effects (a non-empty
/// `items[].effects`, ADR-0065). A version 1 file has no scene filters, and its items'
/// `effects` could only be empty, so it upgrades unchanged; a non-empty list there is
/// refused, as version 1 refused it.
fn v1_to_v2(value: &mut Value) -> Result<(), String> {
    let scenes = value.get("scenes").and_then(Value::as_array);
    for scene in scenes.into_iter().flatten() {
        if scene.get("filters").is_some() {
            return Err("a version 1 scene has no `filters`".into());
        }
        let items = scene.get("items").and_then(Value::as_array);
        for item in items.into_iter().flatten() {
            if item
                .get("effects")
                .and_then(Value::as_array)
                .is_some_and(|e| !e.is_empty())
            {
                return Err("a version 1 item's `effects` must be empty".into());
            }
        }
    }
    Ok(())
}

/// Why a collection file could not be loaded. The message never quotes the file's
/// values beyond IDs, so it is safe to log.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The text is not JSON, or not the shape of its schema version. The reason names
    /// at most a field the file is missing and where the JSON breaks, never a value
    /// from the file: `serde_json`'s own messages quote the offending value, which could
    /// be anything a hand-edited file holds.
    #[error("the collection file does not parse: {0}")]
    Parse(String),
    /// `schema_version` is missing or not a version this build has a migration from.
    #[error(
        "the collection file's schema_version {found} is not supported; this build reads 1 to {SCHEMA_VERSION}"
    )]
    Version {
        /// The version the file has, or `0` when it has none.
        found: u64,
    },
    /// A migration failed.
    #[error("migrating the collection file from schema_version {from}: {reason}")]
    Migration {
        /// The version the failed migration upgrades from.
        from: u64,
        /// What was wrong.
        reason: String,
    },
    /// A builtin source was saved by a version of its kind this build can't read.
    #[error("{object}: kind `{kind}` is version {current} here, but the file has version {saved}")]
    KindVersion {
        /// The source, as `` source `id` ``.
        object: String,
        /// The kind's ID.
        kind: String,
        /// The version in the file.
        saved: u32,
        /// The descriptor's version in this build.
        current: u32,
    },
    /// The collection breaks a rule an edit enforces: the object, as `` scene `id` ``,
    /// and the protocol error rebuilding it got.
    #[error("{object}: {error}")]
    Invalid {
        /// The object being rebuilt.
        object: String,
        /// Why it was refused. Boxed, so a `Result` carrying a `LoadError` stays small.
        error: Box<ProtocolError>,
    },
}

/// The file, at [`SCHEMA_VERSION`]. Its own structs refuse unknown fields, so a file
/// with an object this version doesn't know fails rather than losing it on the next
/// save. The values inside them are the protocol's types, read as the protocol reads
/// them; `the_fixture_is_what_the_collection_saves` fails if one of those changes shape,
/// which is a schema change.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema_version: u32,
    rev: Rev,
    canvas: Canvas,
    /// In display order.
    scenes: Vec<FileScene>,
    /// Ordered by ID.
    sources: Vec<FileSource>,
    /// Ordered by ID.
    outputs: Vec<FileOutput>,
    /// Encoder profiles, ordered by ID; omitted when there are none, so a collection
    /// without profiles saves as it did before they existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    encoders: Vec<FileEncoder>,
    /// Video mixes, ordered by ID; omitted when there are none, like `encoders`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    video_mixes: Vec<FileMix>,
    /// Audio buses, ordered by ID, the master among them; omitted when the master is
    /// the only one and has its defaults, so a collection without audio saves as it
    /// did before buses existed. Missing, the master has its defaults.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    buses: Vec<FileBus>,
    /// Every send, ordered by bus, then by source. Written whenever there is a send or
    /// a source with a channel of its own, so a channel source missing from it has had
    /// its master send removed. Missing (from a file written before sends existed, or
    /// one with no audio), every channel source keeps its default master send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sends: Option<Vec<FileSend>>,
    /// The master stage (ADR-0080); omitted while it has no filters.
    #[serde(default, skip_serializing_if = "FileMaster::is_empty")]
    master: FileMaster,
    /// Folders of the scene list, in folder order; omitted when there are none, like
    /// `labels` (ADR-0024 §3 and §4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    folders: Vec<FileFolder>,
    /// Labels, in label order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    labels: Vec<FileLabel>,
    /// Hotkey bindings (ADR-0024 §5), ordered by chord; omitted when there are none, so
    /// a collection without hotkeys saves as it did before they existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hotkeys: Vec<FileHotkey>,
    /// Transition presets and the default (ADR-0029); omitted while they are the one
    /// `cut` preset a new collection starts with, so such a collection saves as it did
    /// before presets existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transitions: Option<FileTransitions>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTransitions {
    /// Ordered by ID.
    presets: Vec<FilePreset>,
    default: TransitionPresetId,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePreset {
    id: TransitionPresetId,
    name: String,
    kind: String,
    /// The kind descriptor's version the settings follow (ADR-0025, ADR-0037).
    kind_version: u32,
    duration_ms: u32,
    easing: Easing,
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    settings: Settings,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMaster {
    /// Its filter chains; omitted while both are empty.
    #[serde(default, skip_serializing_if = "FileChains::is_empty")]
    filters: FileChains,
}

impl FileMaster {
    fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileScene {
    id: SceneId,
    name: String,
    /// `group` for a group (ADR-0024); omitted for the default, `scene`.
    #[serde(default, skip_serializing_if = "SceneRole::is_scene")]
    role: SceneRole,
    /// The scene's transition overrides (ADR-0029); omitted when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transition_in: Option<TransitionPresetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transition_out: Option<TransitionPresetId>,
    /// The folder it is filed in; omitted for none. The metadata fields below are each
    /// omitted while they have their default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<FolderId>,
    /// The labels it carries, ordered by ID.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    labels: Vec<LabelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    favourite: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    show_in_multiview: bool,
    /// Whether its audio reaches the Mixer when nested (ADR-0089); omitted for `true`.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    include_audio: bool,
    /// Its background colour (ADR-0089 §4); omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    background: Option<String>,
    /// The scene it is a keyframe scene of (ADR-0024 §2); omitted for any other scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    variant_of: Option<SceneId>,
    /// A keyframe scene's overrides, in item ID order; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    overrides: Vec<ItemOverride>,
    /// Its filters (ADR-0065, schema_version 2), a video chain; omitted when there are
    /// none.
    #[serde(default, skip_serializing_if = "FileChains::is_empty")]
    filters: FileChains,
    /// Bottom first.
    items: Vec<FileItem>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFolder {
    id: FolderId,
    name: String,
    /// Omitted at the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<FolderId>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLabel {
    id: LabelId,
    name: String,
    color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileItem {
    id: ItemId,
    source_id: SourceId,
    /// A legacy placement (ADR-0022): exactly one of `transform` and `geometry` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transform: Option<ItemTransform>,
    /// A placement as a box on the canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    geometry: Option<ItemGeometry>,
    crop: Crop,
    visible: bool,
    /// The alpha multiplier (ADR-0023); omitted at 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opacity: Option<Finite32>,
    /// The blend mode; omitted at `normal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blend: Option<BlendMode>,
    /// Its effects (ADR-0065, schema_version 2), first first; omitted when there are none.
    /// A version 1 file could hold only an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<FileFilter>,
    /// Its transition settings (ADR-0029 §6); omitted while they are the defaults.
    #[serde(default, skip_serializing_if = "ItemTransition::is_default")]
    transition: ItemTransition,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSource {
    id: SourceId,
    name: String,
    kind: SourceKind,
    /// The kind descriptor's version the settings follow (ADR-0025). Only `builtin`
    /// sources have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    settings: Settings,
    /// The source this one was copied from (ADR-0023), saved as stored: a plain link
    /// that may name a source since deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duplicated_from: Option<SourceId>,
    /// An audio source's audio settings; omitted when they are its kind's defaults,
    /// except a `webcam`'s, which are always written (#716). A webcam without them was
    /// saved before webcams had audio, and loads with its audio off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audio: Option<SourceAudio>,
    /// The labels it carries, ordered by ID; omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    labels: Vec<LabelId>,
    #[serde(default, skip_serializing_if = "is_false")]
    favourite: bool,
    /// Its filter chains (ADR-0026); omitted while both are empty.
    #[serde(default, skip_serializing_if = "FileChains::is_empty")]
    filters: FileChains,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileChains {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    video: Vec<FileFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    audio: Vec<FileFilter>,
}

impl FileChains {
    fn is_empty(&self) -> bool {
        self.video.is_empty() && self.audio.is_empty()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFilter {
    id: FilterId,
    kind: String,
    /// The kind descriptor's version the settings follow, as for a source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind_version: Option<u32>,
    /// Omitted while enabled.
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    settings: Settings,
}

fn enabled() -> bool {
    true
}

fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileOutput {
    id: OutputId,
    kind: OutputKind,
    /// The output's encoder profile; omitted for the engine's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encoder_profile: Option<EncoderProfileId>,
    /// Its own filter chains (ADR-0080); omitted while both are empty.
    #[serde(default, skip_serializing_if = "FileChains::is_empty")]
    filters: FileChains,
    /// The video mix the output shows; omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    video_mix: Option<VideoMixId>,
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct FileBus {
    id: BusId,
    name: String,
    gain_db: Finite32,
    muted: bool,
    /// The bus's monitor; omitted for none (and in files from before monitors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    monitor: Option<BusMonitor>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSend {
    source_id: SourceId,
    bus_id: BusId,
    gain_db: Finite32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileHotkey {
    chord: Chord,
    /// The action request, as `hotkey.set` takes it. A stream key is never bound, so
    /// none is saved.
    action: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEncoder {
    id: EncoderProfileId,
    name: String,
    profile: EncoderProfile,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMix {
    id: VideoMixId,
    name: String,
    mix: VideoMix,
}

impl Graph {
    /// Both chains as the file saves them.
    fn file_chains(&self, chains: &crate::Chains) -> FileChains {
        FileChains {
            video: self.file_filters(&chains.video),
            audio: self.file_filters(&chains.audio),
        }
    }

    /// One chain as the file saves it.
    fn file_filters(&self, filters: &[crate::Filter]) -> Vec<FileFilter> {
        filters
            .iter()
            .map(|filter| FileFilter {
                id: filter.id.clone(),
                kind: filter.kind.clone(),
                kind_version: match self.kinds.get(&filter.kind) {
                    Some(kind) => Some(kind.version),
                    None => filter.unknown_kind_version,
                },
                enabled: filter.enabled,
                settings: filter.settings.clone(),
            })
            .collect()
    }

    /// The collection as the JSON of a collection file at [`SCHEMA_VERSION`]: its
    /// `rev`, scenes in display order with their items bottom first, sources and outputs
    /// ordered by ID. Pretty-printed, ending in a newline. The kinds are not saved: they
    /// come from the engine.
    pub fn to_collection_json(&self) -> String {
        let state = &self.state;
        let file = File {
            schema_version: SCHEMA_VERSION,
            rev: self.rev,
            canvas: state.canvas,
            scenes: state
                .scenes
                .iter()
                .map(|scene| FileScene {
                    id: scene.id.clone(),
                    name: scene.name.clone(),
                    role: scene.role,
                    transition_in: scene.transition_in.clone(),
                    transition_out: scene.transition_out.clone(),
                    folder: scene.folder.clone(),
                    labels: scene.meta.labels.clone(),
                    icon: scene.meta.icon.clone(),
                    color: scene.meta.color.clone(),
                    favourite: scene.meta.favourite,
                    show_in_multiview: scene.meta.show_in_multiview,
                    include_audio: scene.options.include_audio,
                    background: scene.options.background.clone(),
                    variant_of: scene.variant_of.clone(),
                    overrides: scene
                        .overrides
                        .iter()
                        .map(|(item_id, o)| crate::keyframe::to_info(item_id, o))
                        .collect(),
                    filters: self.file_chains(&scene.filters),
                    items: scene
                        .items
                        .iter()
                        .map(|item| {
                            let (transform, geometry) = match item.placement {
                                Placement::Legacy(transform) => (Some(transform), None),
                                Placement::Geometry(geometry) => (None, Some(geometry)),
                            };
                            FileItem {
                                id: item.id.clone(),
                                source_id: item.source_id.clone(),
                                transform,
                                geometry,
                                crop: item.crop,
                                visible: item.visible,
                                opacity: (item.opacity != Finite32::ONE).then_some(item.opacity),
                                blend: (!item.blend.is_normal()).then_some(item.blend),
                                effects: self.file_filters(&item.effects.video),
                                transition: item.transition,
                            }
                        })
                        .collect(),
                })
                .collect(),
            sources: state
                .sources
                .values()
                .map(|source| FileSource {
                    id: source.id.clone(),
                    name: source.name.clone(),
                    kind: source.kind.clone(),
                    kind_version: match &source.kind {
                        SourceKind::Builtin { kind } => self.kinds.get(kind).map(|k| k.version),
                        _ => None,
                    },
                    settings: source.settings.clone(),
                    duplicated_from: source.duplicated_from.clone(),
                    filters: self.file_chains(&source.filters),
                    // A webcam's audio is always written, even at its default: a webcam
                    // without it can then only come from before webcams had audio (#716),
                    // which loads with its audio off.
                    audio: source.audio.filter(|a| {
                        is_webcam(&source.kind)
                            || Some(*a) != audio::default_audio(&source.kind, &self.kinds)
                    }),
                    labels: source.labels.iter().cloned().collect(),
                    favourite: source.favourite,
                })
                .collect(),
            outputs: state
                .outputs
                .values()
                .map(|output| FileOutput {
                    id: output.id.clone(),
                    kind: output.kind.clone(),
                    encoder_profile: output.encoder_profile.clone(),
                    filters: self.file_chains(&output.filters),
                    video_mix: output.video_mix.clone(),
                })
                .collect(),
            encoders: state
                .encoders
                .values()
                .map(|encoder| FileEncoder {
                    id: encoder.id.clone(),
                    name: encoder.name.clone(),
                    profile: encoder.profile,
                })
                .collect(),
            transitions: (state.transitions != crate::model::Transitions::default()).then(|| {
                FileTransitions {
                    presets: state
                        .transitions
                        .presets
                        .values()
                        .map(|preset| FilePreset {
                            id: preset.id.clone(),
                            name: preset.name.clone(),
                            kind: preset.kind.clone(),
                            kind_version: self.kinds.get(&preset.kind).map_or(1, |k| k.version),
                            duration_ms: preset.duration_ms,
                            easing: preset.easing,
                            settings: preset.settings.clone(),
                        })
                        .collect(),
                    default: state.transitions.default.clone(),
                }
            }),
            video_mixes: state
                .video_mixes
                .values()
                .map(|mix| FileMix {
                    id: mix.id.clone(),
                    name: mix.name.clone(),
                    mix: mix.mix.clone(),
                })
                .collect(),
            buses: file_buses(state),
            sends: file_sends(state),
            master: FileMaster {
                filters: self.file_chains(&state.master),
            },
            folders: state
                .folders
                .iter()
                .map(|folder| FileFolder {
                    id: folder.id.clone(),
                    name: folder.name.clone(),
                    parent: folder.parent.clone(),
                })
                .collect(),
            labels: state
                .labels
                .iter()
                .map(|label| FileLabel {
                    id: label.id.clone(),
                    name: label.name.clone(),
                    color: label.color.clone(),
                    icon: label.icon.clone(),
                })
                .collect(),
            hotkeys: state
                .hotkeys
                .iter()
                .map(|(chord, action)| FileHotkey {
                    chord: chord.clone(),
                    action: action.clone(),
                })
                .collect(),
        };
        let mut json = serde_json::to_string_pretty(&file).expect(
            "invariant: the file holds only strings, finite numbers, bools, and maps with \
             string keys, which always serialize",
        );
        json.push('\n');
        json
    }

    /// This graph, with its state and `rev` replaced by the collection in `json`, a
    /// collection file of any supported schema version. Its kinds are kept, and the
    /// file's `builtin` sources are checked against them.
    ///
    /// # Errors
    /// [`LoadError`] when the file doesn't parse, has an unsupported version, fails a
    /// migration, or holds a collection that an edit would refuse. Nothing is partly
    /// loaded.
    pub fn with_collection_json(self, json: &str) -> Result<Graph, LoadError> {
        let mut value: Value = serde_json::from_str(json).map_err(|e| parse_error(&e))?;
        migrate(&mut value)?;
        let file: File = serde_json::from_value(value).map_err(|e| parse_error(&e))?;
        let mut graph = Graph {
            rev: Rev(0),
            state: Default::default(),
            kinds: self.kinds,
        };
        for (object, edit) in rebuild(&graph, &file)? {
            let applied = match &edit {
                Edit::FilterAdd {
                    owner,
                    chain,
                    filter_id,
                    kind,
                    ..
                } if !graph.kinds.contains_key(kind) => crate::filter::add_unknown_kind(
                    &mut graph.state,
                    &graph.kinds,
                    &edit,
                    file_filter_version(&file, owner, *chain, filter_id),
                ),
                _ => graph.apply(std::slice::from_ref(&edit), None).map(|_| ()),
            };
            applied.map_err(|error| LoadError::Invalid {
                object,
                error: Box::new(error),
            })?;
        }
        graph.rev = file.rev;
        Ok(graph)
    }
}

/// The buses as the file saves them: none when the master is the only one and has its
/// defaults.
fn file_buses(state: &crate::State) -> Vec<FileBus> {
    let buses: Vec<FileBus> = state
        .buses()
        .map(|bus| FileBus {
            id: bus.id().clone(),
            name: bus.name().to_owned(),
            gain_db: bus.gain_db(),
            muted: bus.muted(),
            monitor: bus.monitor().cloned(),
        })
        .collect();
    if buses == [default_master()] {
        Vec::new()
    } else {
        buses
    }
}

/// The master bus of a new collection, as the file saves it.
fn default_master() -> FileBus {
    FileBus {
        id: BusId::master(),
        name: MASTER_NAME.to_owned(),
        gain_db: Finite32::ZERO,
        muted: false,
        monitor: None,
    }
}

/// `error` as a [`LoadError::Parse`] that quotes nothing from the file. A missing field
/// is named (the name is this crate's, not the file's); every other data error, whose
/// `serde_json` message would quote the offending value or key, becomes a fixed reason.
/// A syntax error gives its line and column.
fn parse_error(error: &serde_json::Error) -> LoadError {
    use serde_json::error::Category;
    let reason = match error.classify() {
        Category::Data => {
            let message = error.to_string();
            match message
                .strip_prefix("missing field `")
                .and_then(|rest| rest.split_once('`'))
            {
                Some((field, _)) => format!("missing field `{field}`"),
                None => "a field is unknown, or a value has the wrong type or is out of range"
                    .to_owned(),
            }
        }
        Category::Syntax | Category::Eof | Category::Io => "not valid JSON".to_owned(),
    };
    LoadError::Parse(match error.line() {
        0 => reason,
        line => format!("{reason} at line {line}, column {}", error.column()),
    })
}

/// Upgrades `value` to [`SCHEMA_VERSION`].
fn migrate(value: &mut Value) -> Result<(), LoadError> {
    migrate_with(value, &MIGRATIONS)
}

/// Upgrades `value` through `migrations`, where `migrations[n]` upgrades version `n + 1`
/// to `n + 2`, to version `migrations.len() + 1`, and sets its `schema_version` after each.
fn migrate_with(value: &mut Value, migrations: &[Migration]) -> Result<(), LoadError> {
    let found = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let from = match usize::try_from(found) {
        Ok(v @ 1..) if v <= migrations.len() + 1 => v,
        _ => return Err(LoadError::Version { found }),
    };
    for (from, migration) in (found..).zip(&migrations[from - 1..]) {
        migration(value).map_err(|reason| LoadError::Migration { from, reason })?;
        value["schema_version"] = Value::from(from + 1);
    }
    Ok(())
}

/// The collection's sends for the file: `None` only when there are none and no source
/// has a channel, so a channel source missing from the list is one whose master send
/// was removed.
fn file_sends(state: &crate::State) -> Option<Vec<FileSend>> {
    let sends: Vec<FileSend> = state
        .sends()
        .map(|(source, bus, gain_db)| FileSend {
            source_id: source.clone(),
            bus_id: bus.clone(),
            gain_db,
        })
        .collect();
    let channels = state
        .sources
        .values()
        .any(|source| audio::has_channel(&source.kind, source.audio.as_ref()));
    (channels || !sends.is_empty()).then_some(sends)
}

/// The edits that build `file`'s collection from an empty one, each with the object it
/// creates for the error message. The canvas comes first (when not the default), then
/// folders (top level first, then nested) and labels, then scenes, empty, in display
/// order and with their roles, folders and metadata; then the buses with their faders,
/// which a source's settings may refer to; then sources, each after the sources its
/// settings refer to, with its labels and favourite; then every source's filters; then
/// items, encoder profiles, the transition presets with the default, each scene's
/// transition overrides, video mixes, and outputs, each with its profile, mix and
/// filters; then the master's filters; then the sends (the sources made their default
/// master sends, so when the file lists sends a source without one has it removed), and
/// each source's audio settings; and last the hotkey bindings, whose actions name
/// scenes, outputs and presets.
fn rebuild(graph: &Graph, file: &File) -> Result<Vec<(String, Edit)>, LoadError> {
    let mut edits = Vec::new();
    if file.canvas != graph.state.canvas {
        edits.push((
            "the canvas".to_owned(),
            Edit::CanvasSetSize {
                width: file.canvas.width,
                height: file.canvas.height,
            },
        ));
    }
    // Folders are created top level in folder order, then nested, so a parent may come
    // after its child in the file.
    for folder in &file.folders {
        edits.push((
            format!("folder `{}`", folder.id),
            Edit::FolderCreate {
                folder_id: folder.id.clone(),
                name: folder.name.clone(),
                parent: None,
            },
        ));
    }
    for folder in file.folders.iter().filter(|f| f.parent.is_some()) {
        edits.push((
            format!("folder `{}`", folder.id),
            Edit::FolderSetParent {
                folder_id: folder.id.clone(),
                parent: folder.parent.clone(),
            },
        ));
    }
    for label in &file.labels {
        edits.push((
            format!("label `{}`", label.id),
            Edit::LabelCreate {
                label_id: label.id.clone(),
                name: label.name.clone(),
                color: label.color.clone(),
                icon: label.icon.clone(),
            },
        ));
    }
    // Keyframe scenes are created after every other scene, so a parent may come after
    // its variant in the file; the reorders below then restore the file's order.
    let (variants, parents): (Vec<&FileScene>, Vec<&FileScene>) =
        file.scenes.iter().partition(|s| s.variant_of.is_some());
    for scene in parents.iter().chain(&variants) {
        let object = format!("scene `{}`", scene.id);
        edits.push((
            object.clone(),
            match &scene.variant_of {
                Some(parent) => Edit::SceneCreateVariant {
                    scene_id: scene.id.clone(),
                    variant_of: parent.clone(),
                    name: scene.name.clone(),
                },
                None => Edit::SceneCreate {
                    scene_id: scene.id.clone(),
                    name: scene.name.clone(),
                    role: (!scene.role.is_scene()).then_some(scene.role),
                },
            },
        ));
        // `scene.create_variant` takes no role.
        if scene.variant_of.is_some() && !scene.role.is_scene() {
            edits.push((
                object.clone(),
                Edit::SceneSetRole {
                    scene_id: scene.id.clone(),
                    role: scene.role,
                },
            ));
        }
        if scene.folder.is_some() {
            edits.push((
                object.clone(),
                Edit::SceneSetFolder {
                    scene_id: scene.id.clone(),
                    folder_id: scene.folder.clone(),
                },
            ));
        }
        let meta = SceneMeta {
            labels: scene.labels.clone(),
            icon: scene.icon.clone(),
            color: scene.color.clone(),
            favourite: scene.favourite,
            show_in_multiview: scene.show_in_multiview,
        };
        if !meta.is_default() {
            edits.push((
                object.clone(),
                Edit::SceneSetMeta {
                    scene_id: scene.id.clone(),
                    meta,
                },
            ));
        }
        let options = SceneOptions {
            include_audio: scene.include_audio,
            background: scene.background.clone(),
        };
        if !options.is_default() {
            edits.push((
                object,
                Edit::SceneSetOptions {
                    scene_id: scene.id.clone(),
                    options,
                },
            ));
        }
    }
    let created: Vec<&SceneId> = parents.iter().chain(&variants).map(|s| &s.id).collect();
    if created.iter().zip(&file.scenes).any(|(a, b)| **a != b.id) {
        for (index, scene) in (0u32..).zip(&file.scenes) {
            edits.push((
                format!("scene `{}`", scene.id),
                Edit::SceneReorder {
                    scene_id: scene.id.clone(),
                    index,
                },
            ));
        }
    }
    rebuild_buses(file, &mut edits);
    let sources: BTreeMap<&SourceId, &FileSource> =
        file.sources.iter().map(|s| (&s.id, s)).collect();
    let mut placed = BTreeSet::new();
    for source in &file.sources {
        order_source(graph, &sources, source, &mut placed, &mut edits)?;
    }
    // After every source, so a filter's references find what they name.
    for source in &file.sources {
        let owner = FilterOwner::Source {
            source_id: source.id.clone(),
        };
        let of = format!("source `{}`", source.id);
        add_filters(graph, &owner, &of, &source.filters, &mut edits)?;
    }
    for scene in &file.scenes {
        for item in &scene.items {
            let object = format!("item `{}`", item.id);
            if item.transform.is_some() == item.geometry.is_some() {
                return Err(LoadError::Parse(format!(
                    "{object}: exactly one of `transform` and `geometry` must be set"
                )));
            }
            edits.push((
                object.clone(),
                Edit::ItemAdd {
                    scene_id: scene.id.clone(),
                    item_id: item.id.clone(),
                    source_id: item.source_id.clone(),
                    transform: item.transform,
                    geometry: item.geometry,
                    opacity: item.opacity,
                    blend: item.blend,
                },
            ));
            if item.crop != Crop::NONE {
                edits.push((
                    object.clone(),
                    Edit::ItemSetCrop {
                        item_id: item.id.clone(),
                        scene_id: None,
                        crop: item.crop,
                    },
                ));
            }
            if !item.visible {
                edits.push((
                    object.clone(),
                    Edit::ItemSetVisible {
                        item_id: item.id.clone(),
                        scene_id: None,
                        visible: false,
                    },
                ));
            }
            if !item.transition.is_default() {
                edits.push((
                    object,
                    Edit::ItemSetTransition {
                        item_id: item.id.clone(),
                        transition: item.transition,
                    },
                ));
            }
        }
    }
    // After every scene, item and source, so their references find what they name.
    for scene in &file.scenes {
        let owner = FilterOwner::Scene {
            scene_id: scene.id.clone(),
        };
        let of = format!("scene `{}`", scene.id);
        add_filters(graph, &owner, &of, &scene.filters, &mut edits)?;
        for item in &scene.items {
            let owner = FilterOwner::Item {
                item_id: item.id.clone(),
            };
            let of = format!("item `{}`", item.id);
            add_chain(
                graph,
                &owner,
                &of,
                FilterChain::Video,
                &item.effects,
                &mut edits,
            )?;
        }
    }
    // After every item, since an override names an item in its parent's tree.
    for scene in &file.scenes {
        for o in &scene.overrides {
            let object = format!("scene `{}`", scene.id);
            for edit in override_edits(&scene.id, o) {
                edits.push((object.clone(), edit));
            }
        }
    }
    for encoder in &file.encoders {
        edits.push((
            format!("encoder `{}`", encoder.id),
            Edit::EncoderCreate {
                profile_id: encoder.id.clone(),
                name: encoder.name.clone(),
                profile: encoder.profile,
            },
        ));
    }
    if let Some(transitions) = &file.transitions {
        rebuild_transitions(graph, transitions, &mut edits)?;
    }
    for scene in &file.scenes {
        if scene.transition_in.is_some() || scene.transition_out.is_some() {
            edits.push((
                format!("scene `{}`", scene.id),
                Edit::SceneSetTransitions {
                    scene_id: scene.id.clone(),
                    transition_in: scene.transition_in.clone(),
                    transition_out: scene.transition_out.clone(),
                },
            ));
        }
    }
    for mix in &file.video_mixes {
        edits.push((
            format!("video mix `{}`", mix.id),
            Edit::VideoMixCreate {
                mix_id: mix.id.clone(),
                name: mix.name.clone(),
                mix: mix.mix.clone(),
            },
        ));
    }
    for output in &file.outputs {
        let object = format!("output `{}`", output.id);
        edits.push((
            object.clone(),
            Edit::OutputCreate {
                output_id: output.id.clone(),
                kind: output.kind.clone(),
            },
        ));
        if output.encoder_profile.is_some() {
            edits.push((
                object.clone(),
                Edit::OutputSetEncoder {
                    output_id: output.id.clone(),
                    profile_id: output.encoder_profile.clone(),
                },
            ));
        }
        let owner = FilterOwner::Output {
            output_id: output.id.clone(),
        };
        add_filters(graph, &owner, &object, &output.filters, &mut edits)?;
        if output.video_mix.is_some() {
            edits.push((
                object,
                Edit::OutputSetVideoMix {
                    output_id: output.id.clone(),
                    mix_id: output.video_mix.clone(),
                },
            ));
        }
    }
    let master = &file.master.filters;
    add_filters(
        graph,
        &FilterOwner::Master,
        "the master",
        master,
        &mut edits,
    )?;
    rebuild_audio(graph, file, &mut edits);
    for hotkey in &file.hotkeys {
        edits.push((
            format!("hotkey `{}`", hotkey.chord),
            Edit::HotkeySet {
                chord: hotkey.chord.clone(),
                action: hotkey.action.clone(),
            },
        ));
    }
    Ok(edits)
}

/// The placement edits addressed to the keyframe scene `scene_id` that write `o`.
fn override_edits(scene_id: &SceneId, o: &ItemOverride) -> Vec<Edit> {
    let (item_id, scene_id) = (&o.item_id, Some(scene_id.clone()));
    let mut edits = Vec::new();
    if let Some(geometry) = o.geometry {
        edits.push(Edit::ItemSetGeometry {
            item_id: item_id.clone(),
            scene_id: scene_id.clone(),
            geometry,
        });
    }
    if let Some(transform) = o.transform {
        edits.push(Edit::ItemSetTransform {
            item_id: item_id.clone(),
            scene_id: scene_id.clone(),
            transform,
        });
    }
    if let Some(crop) = o.crop {
        edits.push(Edit::ItemSetCrop {
            item_id: item_id.clone(),
            scene_id: scene_id.clone(),
            crop,
        });
    }
    if let Some(visible) = o.visible {
        edits.push(Edit::ItemSetVisible {
            item_id: item_id.clone(),
            scene_id: scene_id.clone(),
            visible,
        });
    }
    if let Some(opacity) = o.opacity {
        edits.push(Edit::ItemSetOpacity {
            item_id: item_id.clone(),
            scene_id,
            opacity,
        });
    }
    edits
}

/// Pushes the `filter.add`s that rebuild `chains` on `owner`, named `of` in errors,
/// video chain first.
fn add_filters(
    graph: &Graph,
    owner: &FilterOwner,
    of: &str,
    chains: &FileChains,
    edits: &mut Vec<(String, Edit)>,
) -> Result<(), LoadError> {
    add_chain(graph, owner, of, FilterChain::Video, &chains.video, edits)?;
    add_chain(graph, owner, of, FilterChain::Audio, &chains.audio, edits)
}

/// Pushes the `filter.add`s that rebuild one chain, `chain`, of `owner`, first first.
fn add_chain(
    graph: &Graph,
    owner: &FilterOwner,
    of: &str,
    chain: FilterChain,
    filters: &[FileFilter],
    edits: &mut Vec<(String, Edit)>,
) -> Result<(), LoadError> {
    for filter in filters {
        let object = format!("filter `{}` of {of}", filter.id);
        check_kind_version(graph, &object, &filter.kind, filter.kind_version)?;
        edits.push((
            object,
            Edit::FilterAdd {
                owner: owner.clone(),
                chain,
                filter_id: filter.id.clone(),
                kind: filter.kind.clone(),
                settings: filter.settings.clone(),
                enabled: (!filter.enabled).then_some(false),
                index: None,
            },
        ));
    }
    Ok(())
}

/// Refuses settings saved by another version of `kind`'s descriptor (a missing version
/// reads as the current one). A kind the graph doesn't describe is left to the edit.
fn check_kind_version(
    graph: &Graph,
    object: &str,
    kind: &str,
    saved: Option<u32>,
) -> Result<(), LoadError> {
    let Some(descriptor) = graph.kinds.get(kind) else {
        return Ok(());
    };
    let saved = saved.unwrap_or(descriptor.version);
    if saved != descriptor.version {
        return Err(LoadError::KindVersion {
            object: object.to_owned(),
            kind: kind.to_owned(),
            saved,
            current: descriptor.version,
        });
    }
    Ok(())
}

/// Pushes the edits that turn a new collection's one `cut` preset into the file's
/// presets and default: each preset created (the `cut` one updated in place), the
/// default set, and the initial `cut` deleted if the file doesn't have it.
fn rebuild_transitions(
    graph: &Graph,
    file: &FileTransitions,
    edits: &mut Vec<(String, Edit)>,
) -> Result<(), LoadError> {
    let initial = TransitionPresetId::new(INITIAL_PRESET);
    for preset in &file.presets {
        let object = format!("transition `{}`", preset.id);
        check_kind_version(graph, &object, &preset.kind, Some(preset.kind_version))?;
        let edit = if preset.id == initial {
            Edit::TransitionUpdate {
                preset_id: preset.id.clone(),
                name: Some(preset.name.clone()),
                kind: Some(preset.kind.clone()),
                duration_ms: Some(preset.duration_ms),
                easing: Some(preset.easing),
                patch: preset.settings.clone(),
            }
        } else {
            Edit::TransitionCreate {
                preset_id: preset.id.clone(),
                name: preset.name.clone(),
                kind: preset.kind.clone(),
                duration_ms: preset.duration_ms,
                easing: preset.easing,
                settings: preset.settings.clone(),
            }
        };
        edits.push((object, edit));
    }
    edits.push((
        "the default transition".to_owned(),
        Edit::TransitionSetDefault {
            preset_id: file.default.clone(),
        },
    ));
    if !file.presets.iter().any(|preset| preset.id == initial) {
        edits.push((
            format!("transition `{initial}`"),
            Edit::TransitionDelete { preset_id: initial },
        ));
    }
    Ok(())
}

/// The kind version `file` recorded for `filter_id` on `chain` of `owner`.
fn file_filter_version(
    file: &File,
    owner: &FilterOwner,
    chain: FilterChain,
    filter_id: &FilterId,
) -> Option<u32> {
    let chains = match owner {
        FilterOwner::Source { source_id } => {
            &file.sources.iter().find(|s| &s.id == source_id)?.filters
        }
        FilterOwner::Output { output_id } => {
            &file.outputs.iter().find(|o| &o.id == output_id)?.filters
        }
        FilterOwner::Master => &file.master.filters,
        FilterOwner::Scene { scene_id } => &file.scenes.iter().find(|s| &s.id == scene_id)?.filters,
        // An item's effects are one video chain (ADR-0065 §1).
        FilterOwner::Item { item_id } => {
            let effects = &file
                .scenes
                .iter()
                .flat_map(|s| &s.items)
                .find(|i| &i.id == item_id)?
                .effects;
            return match chain {
                FilterChain::Video => effects.iter().find(|f| &f.id == filter_id)?.kind_version,
                FilterChain::Audio => None,
            };
        }
        _ => return None,
    };
    let filters = match chain {
        FilterChain::Video => &chains.video,
        FilterChain::Audio => &chains.audio,
    };
    filters.iter().find(|f| &f.id == filter_id)?.kind_version
}

/// Pushes the edits that give the rebuilt collection `file`'s buses, their faders and
/// monitors, before its sources: a bus depends on nothing, and a source's settings may
/// refer to one (a `waveform`'s `audio`).
fn rebuild_buses(file: &File, edits: &mut Vec<(String, Edit)>) {
    let default = default_master();
    for bus in &file.buses {
        let object = format!("bus `{}`", bus.id);
        if bus.id.is_master() {
            if bus.name != default.name {
                edits.push((
                    object.clone(),
                    Edit::BusRename {
                        bus_id: bus.id.clone(),
                        name: bus.name.clone(),
                    },
                ));
            }
        } else {
            edits.push((
                object.clone(),
                Edit::BusCreate {
                    bus_id: bus.id.clone(),
                    name: bus.name.clone(),
                },
            ));
        }
        if bus.gain_db != default.gain_db || bus.muted != default.muted {
            edits.push((
                object,
                Edit::BusSetFader {
                    bus_id: bus.id.clone(),
                    gain_db: Some(bus.gain_db),
                    muted: Some(bus.muted),
                },
            ));
        }
        if let Some(monitor) = &bus.monitor {
            edits.push((
                format!("bus `{}`", bus.id),
                Edit::BusSetMonitor {
                    bus_id: bus.id.clone(),
                    monitor: Some(monitor.clone()),
                },
            ));
        }
    }
}

/// Pushes the edits that give the rebuilt collection `file`'s sends and source audio
/// settings, after its sources exist.
fn rebuild_audio(graph: &Graph, file: &File, edits: &mut Vec<(String, Edit)>) {
    for source in &file.sources {
        let object = format!("source `{}`", source.id);
        let default = audio::default_audio(&source.kind, &graph.kinds);
        let gets_master = default.is_some() && !matches!(source.kind, SourceKind::Scene { .. });
        // A `webcam` without audio settings was saved before webcams had audio (#716):
        // since then a webcam's are always written. It has no send either (a source
        // without audio settings can't have one). It keeps the master send its creation
        // gives it, but loads with its audio off, so opening an older collection never
        // starts recording a webcam's microphone; turning it on sends it to the master.
        let unheard_webcam = is_webcam(&source.kind) && source.audio.is_none();
        // A file without `sends` predates them (or has no audio): every channel source
        // keeps the master send its creation gave it.
        let keeps_master = unheard_webcam
            || file.sends.as_ref().is_none_or(|sends| {
                sends
                    .iter()
                    .any(|send| send.source_id == source.id && send.bus_id.is_master())
            });
        if gets_master && !keeps_master {
            edits.push((
                object.clone(),
                Edit::SendRemove {
                    source_id: source.id.clone(),
                    bus_id: BusId::master(),
                },
            ));
        }
        if unheard_webcam && default.is_some() {
            edits.push((
                object.clone(),
                Edit::SourceSetAudio {
                    source_id: source.id.clone(),
                    enabled: Some(false),
                    follow: None,
                    gain_db: None,
                    muted: None,
                },
            ));
        }
        if let Some(audio) = source.audio {
            let scene = matches!(source.kind, SourceKind::Scene { .. });
            edits.push((
                object,
                Edit::SourceSetAudio {
                    source_id: source.id.clone(),
                    enabled: Some(audio.enabled),
                    follow: Some(audio.follow),
                    gain_db: (!scene).then_some(audio.gain_db),
                    muted: (!scene).then_some(audio.muted),
                },
            ));
        }
    }
    for send in file.sends.iter().flatten() {
        edits.push((
            format!("send from `{}` to `{}`", send.source_id, send.bus_id),
            Edit::SendSet {
                source_id: send.source_id.clone(),
                bus_id: send.bus_id.clone(),
                gain_db: send.gain_db,
            },
        ));
    }
}

/// Pushes `source.create` for `source`, after the sources its settings refer to. A
/// source already placed, or being placed (a reference cycle, which the edit then
/// refuses), is skipped.
fn order_source<'a>(
    graph: &Graph,
    sources: &BTreeMap<&SourceId, &'a FileSource>,
    source: &'a FileSource,
    placed: &mut BTreeSet<&'a SourceId>,
    edits: &mut Vec<(String, Edit)>,
) -> Result<(), LoadError> {
    if !placed.insert(&source.id) {
        return Ok(());
    }
    for (_, reference) in source.settings.references() {
        if let jstream_protocol::Reference::Source { source_id } = reference
            && let Some(target) = sources.get(&source_id)
        {
            order_source(graph, sources, target, placed, edits)?;
        }
    }
    let object = format!("source `{}`", source.id);
    if let SourceKind::Builtin { kind } = &source.kind {
        check_kind_version(graph, &object, kind, source.kind_version)?;
    }
    edits.push((
        object.clone(),
        Edit::SourceCreate {
            source_id: source.id.clone(),
            name: source.name.clone(),
            kind: source.kind.clone(),
            settings: source.settings.clone(),
            duplicated_from: source.duplicated_from.clone(),
        },
    ));
    let meta = SourceMeta {
        labels: source.labels.clone(),
        favourite: source.favourite,
    };
    if !meta.is_default() {
        edits.push((
            object,
            Edit::SourceSetMeta {
                source_id: source.id.clone(),
                meta,
            },
        ));
    }
    Ok(())
}
