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

use crate::{Graph, Placement};
use jstream_protocol::{
    BlendMode, Canvas, Crop, Edit, EncoderProfile, EncoderProfileId, FilterChain, FilterId,
    FilterOwner, Finite32, ItemGeometry, ItemId, ItemTransform, OutputId, OutputKind,
    ProtocolError, Rev, SceneId, SceneRole, Settings, SourceId, SourceKind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

/// The collection file's current `schema_version`. Bumping it ships a migration in the
/// crate's private `MIGRATIONS` table and a fixture of the previous version that a test
/// loads (R-API-003).
pub const SCHEMA_VERSION: u32 = 1;

/// A migration upgrades a file's JSON from one schema version to the next, in place.
type Migration = fn(&mut Value) -> Result<(), String>;

/// `MIGRATIONS[n]` upgrades version `n + 1` to `n + 2`, so a file at version `v` runs
/// `MIGRATIONS[v - 1..]`. Version 1 is the first, so there are none yet.
const MIGRATIONS: [Migration; SCHEMA_VERSION as usize - 1] = [];

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
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileScene {
    id: SceneId,
    name: String,
    /// `group` for a group (ADR-0024); omitted for the default, `scene`.
    #[serde(default, skip_serializing_if = "SceneRole::is_scene")]
    role: SceneRole,
    /// Bottom first.
    items: Vec<FileItem>,
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
    /// Per-placement effects, reserved (ADR-0023 §1): accepted so that item effects can
    /// come later without a migration, never written, and refused unless empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Value>,
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
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEncoder {
    id: EncoderProfileId,
    name: String,
    profile: EncoderProfile,
}

impl Graph {
    /// One chain as the file saves it.
    fn file_filters(&self, filters: &[crate::Filter]) -> Vec<FileFilter> {
        filters
            .iter()
            .map(|filter| FileFilter {
                id: filter.id.clone(),
                kind: filter.kind.clone(),
                kind_version: self.kinds.get(&filter.kind).map(|k| k.version),
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
                                effects: Vec::new(),
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
                    filters: FileChains {
                        video: self.file_filters(&source.filters.video),
                        audio: self.file_filters(&source.filters.audio),
                    },
                })
                .collect(),
            outputs: state
                .outputs
                .values()
                .map(|output| FileOutput {
                    id: output.id.clone(),
                    kind: output.kind.clone(),
                    encoder_profile: output.encoder_profile.clone(),
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
            graph
                .apply(std::slice::from_ref(&edit), None)
                .map_err(|error| LoadError::Invalid {
                    object,
                    error: Box::new(error),
                })?;
        }
        graph.rev = file.rev;
        Ok(graph)
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

/// The edits that build `file`'s collection from an empty one, each with the object it
/// creates for the error message. The canvas comes first (when not the default), then
/// scenes, empty, in display order and with their roles; then sources, each after the
/// sources its settings refer to; then every source's filters; then items, encoder profiles, and outputs, each with
/// its profile.
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
    for scene in &file.scenes {
        edits.push((
            format!("scene `{}`", scene.id),
            Edit::SceneCreate {
                scene_id: scene.id.clone(),
                name: scene.name.clone(),
                role: (!scene.role.is_scene()).then_some(scene.role),
            },
        ));
    }
    let sources: BTreeMap<&SourceId, &FileSource> =
        file.sources.iter().map(|s| (&s.id, s)).collect();
    let mut placed = BTreeSet::new();
    for source in &file.sources {
        order_source(graph, &sources, source, &mut placed, &mut edits)?;
    }
    // After every source, so a filter's references find what they name.
    for source in &file.sources {
        let chains = [
            (FilterChain::Video, &source.filters.video),
            (FilterChain::Audio, &source.filters.audio),
        ];
        for (chain, filters) in chains {
            for filter in filters {
                let object = format!("filter `{}` of source `{}`", filter.id, source.id);
                check_kind_version(graph, &object, &filter.kind, filter.kind_version)?;
                edits.push((
                    object,
                    Edit::FilterAdd {
                        owner: FilterOwner::Source {
                            source_id: source.id.clone(),
                        },
                        chain,
                        filter_id: filter.id.clone(),
                        kind: filter.kind.clone(),
                        settings: filter.settings.clone(),
                        enabled: (!filter.enabled).then_some(false),
                        index: None,
                    },
                ));
            }
        }
    }
    for scene in &file.scenes {
        for item in &scene.items {
            let object = format!("item `{}`", item.id);
            if item.transform.is_some() == item.geometry.is_some() {
                return Err(LoadError::Parse(format!(
                    "{object}: exactly one of `transform` and `geometry` must be set"
                )));
            }
            if !item.effects.is_empty() {
                return Err(LoadError::Parse(format!(
                    "{object}: `effects` is reserved and must be empty"
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
                    object,
                    Edit::ItemSetVisible {
                        item_id: item.id.clone(),
                        scene_id: None,
                        visible: false,
                    },
                ));
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
                object,
                Edit::OutputSetEncoder {
                    output_id: output.id.clone(),
                    profile_id: output.encoder_profile.clone(),
                },
            ));
        }
    }
    Ok(edits)
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
        object,
        Edit::SourceCreate {
            source_id: source.id.clone(),
            name: source.name.clone(),
            kind: source.kind.clone(),
            settings: source.settings.clone(),
            duplicated_from: source.duplicated_from.clone(),
        },
    ));
    Ok(())
}
