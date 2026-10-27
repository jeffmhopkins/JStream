//! Folders, labels and the organisation metadata of scenes and sources
//! ([ADR-0024](../../../docs/adr/0024-scene-structure.md) §3 and §4).
//!
//! All of it is collection state: edited by undoable edits, seen by every client and
//! agent, and saved with the collection. None of it changes rendering, switching or
//! nesting. Folders organise the scene list as a tree; scenes keep their flat display
//! order, and a folder's scenes are the scenes that name it, in that order. Labels are
//! an ordered list that sources and scenes carry; a source's labels belong to the
//! source, so every item placing it shows them.
//!
//! The checks every engine and the mock run on these values are here
//! ([`check_icon`], [`check_color`]), so they reject the same ones.

use crate::id::{FolderId, LabelId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The longest `icon` name, in bytes.
pub const MAX_ICON_LEN: usize = 64;

/// One folder of the scene list, as `state.get` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FolderInfo {
    /// The folder's ID.
    pub folder_id: FolderId,
    /// Its display name.
    pub name: String,
    /// The folder it sits in. Omitted for a top-level folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<FolderId>,
}

/// One label, as `state.get` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LabelInfo {
    /// The label's ID.
    pub label_id: LabelId,
    /// Its display name.
    pub name: String,
    /// Its colour, sRGB as `#rrggbb`.
    pub color: String,
    /// An icon name from the UI's icon set. The engine stores it and checks only its
    /// length. Omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// A scene's organisation metadata: what `scene.set_meta` sets, and what `scene.list`
/// and `state.get` report beside the scene's other fields. Each field left out takes
/// its default, so a scene with none set reports none of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SceneMeta {
    /// The labels it carries, ordered by ID, each at most once. Every one must exist.
    /// Omitted for none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<LabelId>,
    /// An icon name from the UI's icon set, at most 64 bytes. Omitted for none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The scene's colour, sRGB as `#rrggbb`. Omitted for none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Whether it is a favourite. JStream is single-user, so this is not per person.
    /// Omitted for `false`.
    #[serde(skip_serializing_if = "is_false")]
    pub favourite: bool,
    /// Whether a multiview shows it. Omitted for `true`, the default.
    #[serde(skip_serializing_if = "is_true")]
    pub show_in_multiview: bool,
}

impl Default for SceneMeta {
    fn default() -> Self {
        SceneMeta {
            labels: Vec::new(),
            icon: None,
            color: None,
            favourite: false,
            show_in_multiview: true,
        }
    }
}

impl SceneMeta {
    /// Whether every field has its default, so the scene carries no metadata.
    pub fn is_default(&self) -> bool {
        *self == SceneMeta::default()
    }

    /// Why an engine refuses this metadata, as the field and the reason, or `None`.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        if let Some(icon) = &self.icon
            && let Err(reason) = check_icon(icon)
        {
            return Some(("icon", reason));
        }
        if let Some(color) = &self.color
            && let Err(reason) = check_color(color)
        {
            return Some(("color", reason));
        }
        None
    }
}

/// A source's organisation metadata: what `source.set_meta` sets, and what
/// `source.list` and `state.get` report beside the source's other fields. It belongs to
/// the source (ADR-0023), so every item placing the source shares it, and
/// `source.duplicate` copies it to the copy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SourceMeta {
    /// The labels it carries, ordered by ID, each at most once. Every one must exist.
    /// Omitted for none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<LabelId>,
    /// Whether it is a favourite. Omitted for `false`.
    #[serde(skip_serializing_if = "is_false")]
    pub favourite: bool,
}

impl SourceMeta {
    /// Whether every field has its default, so the source carries no metadata.
    pub fn is_default(&self) -> bool {
        self.labels.is_empty() && !self.favourite
    }
}

/// Checks an `icon` name's length: 1 to [`MAX_ICON_LEN`] bytes. The name is opaque to
/// the engine (ADR-0024 §4), so nothing else about it is checked.
pub fn check_icon(icon: &str) -> Result<(), String> {
    if icon.is_empty() || icon.len() > MAX_ICON_LEN {
        return Err(format!("must be 1 to {MAX_ICON_LEN} bytes"));
    }
    Ok(())
}

/// Checks a colour: sRGB as `#rrggbb`, in either case.
pub fn check_color(color: &str) -> Result<(), String> {
    let ok = color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err("must be a colour as `#rrggbb`".into())
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

#[cfg(test)]
mod tests {
    use super::{SceneMeta, SourceMeta, check_color, check_icon};
    use crate::LabelId;
    use serde_json::json;

    #[test]
    fn default_metadata_is_omitted_and_missing_fields_take_defaults() {
        assert_eq!(
            serde_json::to_value(SceneMeta::default()).expect("serializes"),
            json!({})
        );
        let meta: SceneMeta = serde_json::from_value(json!({"favourite": true})).expect("parses");
        assert!(meta.show_in_multiview && meta.favourite);
        let hidden: SceneMeta =
            serde_json::from_value(json!({"show_in_multiview": false})).expect("parses");
        assert_eq!(
            serde_json::to_value(&hidden).expect("serializes"),
            json!({"show_in_multiview": false})
        );
        let source = SourceMeta {
            labels: vec![LabelId::new("sponsor")],
            favourite: false,
        };
        assert_eq!(
            serde_json::to_value(&source).expect("serializes"),
            json!({"labels": ["sponsor"]})
        );
        assert!(SourceMeta::default().is_default());
    }

    #[test]
    fn icons_and_colours_are_checked() {
        assert!(check_icon("mic").is_ok() && check_icon("two words").is_ok());
        for bad in ["", &"x".repeat(65)] {
            assert!(check_icon(bad).is_err(), "{bad:?}");
        }
        assert!(check_color("#1E1e2e").is_ok());
        for bad in ["", "1e1e2e", "#1e1e2", "#1e1e2eff", "#gggggg", "#ééé"] {
            assert!(check_color(bad).is_err(), "{bad:?}");
        }
        let meta = SceneMeta {
            color: Some("red".into()),
            ..SceneMeta::default()
        };
        assert_eq!(meta.problem().map(|(f, _)| f), Some("color"));
    }
}
