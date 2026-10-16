//! Settings maps and the typed references inside them (ADR-0025,
//! docs/adr/0025-typed-settings-kind-descriptors.md).
//!
//! A settings map goes from parameter ID to a plain JSON value. Scalars are plain JSON:
//! booleans, numbers, strings, and colours as `#rrggbb` or `#rrggbbaa` strings.
//! References are tagged objects ([`Reference`]), so the graph finds every reference in
//! any map without reading the kind's descriptor.

use crate::id::{SceneId, SourceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A source's (or later a filter's) settings: parameter ID to value.
///
/// The values follow the kind's descriptor (`kind.describe`). The engine validates every
/// edit against it and stores the complete map, so a reported map holds every parameter
/// of the kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Settings(pub BTreeMap<String, Value>);

impl Settings {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value of parameter `id`.
    pub fn get(&self, id: &str) -> Option<&Value> {
        self.0.get(id)
    }

    /// Every entry, ordered by parameter ID.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Sets parameter `id`, returning the value it replaced.
    pub fn insert(&mut self, id: impl Into<String>, value: Value) -> Option<Value> {
        self.0.insert(id.into(), value)
    }

    /// Every typed reference in the map, with the parameter that holds it, ordered by
    /// parameter ID. A value whose tag isn't `scene`, `source` or `asset`, or that
    /// doesn't parse as one, is not a reference.
    pub fn references(&self) -> impl Iterator<Item = (&str, Reference)> {
        self.iter()
            .filter_map(|(id, value)| Reference::from_value(value).map(|r| (id, r)))
    }
}

impl<K: Into<String>> FromIterator<(K, Value)> for Settings {
    fn from_iter<I: IntoIterator<Item = (K, Value)>>(iter: I) -> Self {
        Settings(iter.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
}

/// A typed reference held by a settings value.
///
/// Scene and source references are dependency edges: the graph rejects a reference that
/// would make a cycle, and refuses to delete what is referenced. Asset references name
/// files, which are outside the collection state, so they are not edges.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Reference {
    /// A scene (a mask or matte).
    Scene {
        /// The scene referred to.
        scene_id: SceneId,
    },
    /// A source (a ducking side-chain, an audio visualiser's input).
    Source {
        /// The source referred to.
        source_id: SourceId,
    },
    /// A file: a root and a path under it.
    Asset {
        /// Which folder `path` is under.
        root: AssetRoot,
        /// Under `collection` or `app`, a relative path with `/` separators, no `..`
        /// segment, no drive letter and no leading `/`. Under `absolute`, an absolute
        /// path that is not a UNC or device path (no leading `\\` or `//`), with no
        /// `:` but the drive letter's, no `?` or `*` (so no `\??\…` NT object path), and
        /// no DOS device name (`CON`, `NUL.png`, `COM1`, …) as its last component.
        /// [`check_asset_path`](crate::check_asset_path) holds the full rules.
        path: String,
    },
}

impl Reference {
    /// The reference a settings value holds, if it is an object tagged `scene`, `source`
    /// or `asset` that parses as one.
    pub fn from_value(value: &Value) -> Option<Reference> {
        let tag = value.as_object()?.get("type")?.as_str()?;
        if !matches!(tag, "scene" | "source" | "asset") {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }

    /// The reference as a settings value.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The folder an asset path is relative to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AssetRoot {
    /// The collection file's folder.
    Collection,
    /// The app's asset folder (inside the portable folder in portable mode).
    App,
    /// No root: `path` is absolute.
    Absolute,
}

#[cfg(test)]
mod tests {
    use super::{AssetRoot, Reference, Settings};
    use crate::id::SceneId;
    use serde_json::json;

    #[test]
    fn references_are_found_without_a_descriptor() {
        let settings: Settings = [
            ("color", json!("#ff0000")),
            ("mask", json!({"type": "scene", "scene_id": "m"})),
            (
                "file",
                json!({"type": "asset", "root": "collection", "path": "a/b.png"}),
            ),
            ("other", json!({"type": "teleport", "to": "x"})),
            ("none", json!(null)),
        ]
        .into_iter()
        .collect();
        let found: Vec<_> = settings.references().collect();
        assert_eq!(
            found,
            vec![
                (
                    "file",
                    Reference::Asset {
                        root: AssetRoot::Collection,
                        path: "a/b.png".into()
                    }
                ),
                (
                    "mask",
                    Reference::Scene {
                        scene_id: SceneId::new("m")
                    }
                ),
            ]
        );
    }

    #[test]
    fn wire_form_is_a_plain_object() {
        let settings: Settings = [("size", json!(3))].into_iter().collect();
        assert_eq!(
            serde_json::to_value(&settings).ok(),
            Some(json!({"size": 3}))
        );
    }
}
