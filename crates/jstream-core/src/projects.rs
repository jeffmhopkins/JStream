//! Projects (ADR-0121): the pieces of `collection.open`, `collection.new` and
//! `collection.save_as` that need no engine lock. The path rules, the stream keys' scope
//! names, the recent list the host names a file for, and the rewrite that keeps
//! `collection` asset references naming the same files after a save as.
//!
//! Docs: crates/jstream-core/README.md#design, docs/ARCHITECTURE.md#persistence

use jstream_protocol::{AssetRoot, OutputId, ProtocolError, check_asset_path};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// How many projects `collection.info` lists as recent.
pub(crate) const MAX_RECENT: usize = 10;

/// `path`, as `collection.open`, `collection.new` and `collection.save_as` take it: an
/// absolute path that follows the `absolute` asset path rules, so it names no UNC
/// share, device or NT object path (ADR-0121 §2).
///
/// # Errors
/// `invalid_argument` on `path` otherwise.
pub(crate) fn checked_path(path: &str) -> Result<PathBuf, ProtocolError> {
    check_asset_path(AssetRoot::Absolute, path)
        .map_err(|reason| ProtocolError::invalid_argument("path", reason))?;
    let checked = PathBuf::from(path);
    if !checked.is_absolute() {
        return Err(ProtocolError::invalid_argument(
            "path",
            "the path must be absolute on the engine's machine",
        ));
    }
    if checked.file_name().is_none() {
        return Err(ProtocolError::invalid_argument(
            "path",
            "the path names no file",
        ));
    }
    Ok(checked)
}

/// The stream-key scope of the project saved at `path` (ADR-0121 §5): a hash of the
/// path, its folder resolved through any links, so it is the same before and after the
/// file is made. Only the engine's machine decides it: nothing in a project file names
/// it, so a file someone else hands over never reaches the keys of a project of one's
/// own at another path.
pub(crate) fn key_scope(path: &Path) -> String {
    let path = absolute(path);
    let resolved = match (path.parent(), path.file_name()) {
        (Some(folder), Some(name)) => fs::canonicalize(folder)
            .map(|folder| folder.join(name))
            .unwrap_or(path),
        _ => path,
    };
    let hash = blake3::hash(resolved.as_os_str().as_encoded_bytes());
    hash.as_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The name the key store holds `output_id`'s key under: `<scope>/<output_id>`, or the
/// bare output ID for a collection whose keys aren't scoped yet.
pub(crate) fn key_name(scope: Option<&str>, output_id: &OutputId) -> String {
    match scope {
        Some(scope) => format!("{scope}/{output_id}"),
        None => output_id.as_str().to_owned(),
    }
}

/// `path` made absolute against the current directory, or as it is if that fails.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The path as the protocol reports it.
pub(crate) fn path_text(path: &Path) -> String {
    path.display().to_string()
}

/// The projects in the recent list kept at `path` (ADR-0121 §6), most recent first:
/// where a host looks for the project to open at startup. A missing or unreadable file
/// lists none.
pub fn recent_collections(path: &Path) -> Vec<PathBuf> {
    Recent::load(path.to_path_buf())
        .list
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// The projects opened most recently, kept in the file the host names
/// (`recent-collections.json` in the data folder).
pub(crate) struct Recent {
    path: PathBuf,
    list: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RecentFile {
    version: u32,
    #[serde(default)]
    recent: Vec<String>,
}

impl Recent {
    /// The list kept at `path`. A file that can't be read or parsed is logged and read
    /// as empty; the next change replaces it.
    pub(crate) fn load(path: PathBuf) -> Recent {
        let list = match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<RecentFile>(&text) {
                Ok(file) => file.recent.into_iter().take(MAX_RECENT).collect(),
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "recent projects file not read");
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "recent projects file not read");
                Vec::new()
            }
        };
        Recent { path, list }
    }

    /// The list, most recent first.
    pub(crate) fn list(&self) -> &[String] {
        &self.list
    }

    /// Puts `project` first, drops a repeat of it and anything past [`MAX_RECENT`], and
    /// saves the list. A failed save is logged: the list is still kept in memory.
    pub(crate) fn push(&mut self, project: String) {
        self.list.retain(|p| *p != project);
        self.list.insert(0, project);
        self.list.truncate(MAX_RECENT);
        let file = RecentFile {
            version: 1,
            recent: self.list.clone(),
        };
        let mut json = serde_json::to_string_pretty(&file)
            .expect("invariant: a list of strings always serializes");
        json.push('\n');
        if let Err(err) = crate::collection::write_atomically(&self.path, json.as_bytes()) {
            tracing::error!(path = %self.path.display(), error = %err, "recent projects not saved");
        }
    }
}

/// Rewrites, in place, the asset references in a collection file's `json` for a save
/// as from the folder `from` to the folder `to` (ADR-0121 §4): a `collection` reference
/// keeps naming the same file, relative to `to` when the file is under it and otherwise
/// `absolute`, and an `absolute` reference to a file under `to` becomes relative to it.
/// Returns how many it rewrote. Both folders are absolute. A reference whose new path
/// the asset rules would refuse is left as it is and logged.
pub(crate) fn move_collection_assets(json: &mut Value, from: &Path, to: &Path) -> usize {
    let mut moved = 0;
    walk(json, &mut |reference| {
        let (Some(root), Some(old)) = (
            reference.get("root").and_then(Value::as_str),
            reference.get("path").and_then(Value::as_str),
        ) else {
            return;
        };
        let file = match root {
            "collection" => old.split('/').fold(from.to_path_buf(), |p, s| p.join(s)),
            "absolute" => PathBuf::from(old),
            _ => return,
        };
        let (new_root, path) = match relative_under(&file, to) {
            Some(rel) => (AssetRoot::Collection, rel),
            None if root == "absolute" => return,
            None => (AssetRoot::Absolute, path_text(&file)),
        };
        let name = if new_root == AssetRoot::Collection {
            "collection"
        } else {
            "absolute"
        };
        if name == root && path == old {
            return;
        }
        if let Err(reason) = check_asset_path(new_root, &path) {
            tracing::warn!(asset = %old, %reason, "asset reference not moved");
            return;
        }
        reference["root"] = Value::from(name);
        reference["path"] = Value::from(path);
        moved += 1;
    });
    moved
}

/// Calls `found` on every `{"type": "asset", …}` object in `value`.
fn walk(value: &mut Value, found: &mut impl FnMut(&mut Value)) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("asset") {
                found(value);
            } else {
                for child in map.values_mut() {
                    walk(child, found);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| walk(item, found)),
        _ => {}
    }
}

/// `file` relative to `folder`, with `/` separators, when it is under it.
fn relative_under(file: &Path, folder: &Path) -> Option<String> {
    let rest = file.strip_prefix(folder).ok()?;
    let mut parts = Vec::new();
    for component in rest.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root() -> PathBuf {
        std::path::absolute("/").expect("root")
    }

    #[test]
    fn a_path_must_be_absolute_and_name_a_file() {
        assert!(checked_path("show.json").is_err());
        assert!(checked_path("../show.json").is_err());
        assert!(checked_path("//host/share/show.json").is_err());
        #[cfg(unix)]
        assert!(checked_path("/home/me/show.json").is_ok());
        #[cfg(windows)]
        assert!(checked_path("C:/Users/me/show.json").is_ok());
    }

    #[test]
    fn key_names_carry_the_scope() {
        let id = OutputId::new("out-1");
        assert_eq!(key_name(None, &id), "out-1");
        assert_eq!(key_name(Some("ab12"), &id), "ab12/out-1");
        let scope = key_scope(&root().join("shows").join("a.json"));
        assert_eq!(scope.len(), 32);
        assert_eq!(scope, key_scope(&root().join("shows").join("a.json")));
        assert_ne!(scope, key_scope(&root().join("shows").join("b.json")));
    }

    #[test]
    fn save_as_keeps_collection_assets_on_the_same_files() {
        let from = root().join("shows").join("a");
        let mut json = json!({
            "sources": [
                {"settings": {"file": {"type": "asset", "root": "collection", "path": "img/logo.png"}}},
                {"settings": {"file": {"type": "asset", "root": "app", "path": "x.png"}}},
                {"settings": {"list": [{"type": "asset", "root": "collection", "path": "clip.mp4"}]}},
            ]
        });
        // Into a folder above: still under it, so still relative.
        let mut up = json.clone();
        assert_eq!(
            move_collection_assets(&mut up, &from, &root().join("shows")),
            2
        );
        assert_eq!(
            up["sources"][0]["settings"]["file"]["path"],
            "a/img/logo.png"
        );
        assert_eq!(up["sources"][0]["settings"]["file"]["root"], "collection");
        assert_eq!(up["sources"][1]["settings"]["file"]["root"], "app");
        // Into another folder: absolute.
        let elsewhere = root().join("other");
        assert_eq!(move_collection_assets(&mut json, &from, &elsewhere), 2);
        let file = &json["sources"][2]["settings"]["list"][0];
        assert_eq!(file["root"], "absolute");
        assert_eq!(file["path"], path_text(&from.join("clip.mp4")));
        // An absolute reference under the new folder becomes relative to it.
        let mut abs = json!({"a": {"type": "asset", "root": "absolute",
            "path": path_text(&elsewhere.join("b.png"))}});
        assert_eq!(move_collection_assets(&mut abs, &from, &elsewhere), 1);
        assert_eq!(
            abs["a"],
            json!({"type": "asset", "root": "collection", "path": "b.png"})
        );
        // The same folder changes nothing.
        let mut same = json!({"a": {"type": "asset", "root": "collection", "path": "b.png"}});
        assert_eq!(move_collection_assets(&mut same, &from, &from), 0);
    }

    #[test]
    fn the_recent_list_puts_the_latest_first_and_keeps_ten() {
        let dir = std::env::temp_dir().join(format!("jstream-recent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("recent-collections.json");
        let mut recent = Recent::load(path.clone());
        for n in 0..12 {
            recent.push(format!("/p/{n}.json"));
        }
        recent.push("/p/5.json".into());
        assert_eq!(recent.list().len(), MAX_RECENT);
        assert_eq!(recent.list()[0], "/p/5.json");
        assert_eq!(recent.list()[1], "/p/11.json");
        assert_eq!(Recent::load(path.clone()).list(), recent.list());
        fs::write(&path, "not json").expect("write");
        assert!(Recent::load(path).list().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
