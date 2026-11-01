//! The UI settings the server keeps for its clients (ADR-0084): `ui_settings.get` and
//! `ui_settings.set`, so the desktop app and every browser of one engine show the same
//! workspace.
//!
//! - **Opaque values.** A setting is any JSON value under a key; the UI owns its shape
//!   (the web UI keeps its dock under `workspace.dock`). The server checks only the key's
//!   form ([`check_key`]) and the sizes: a value of at most [`MAX_VALUE_BYTES`] as compact
//!   JSON, and at most [`MAX_KEYS`] keys, so the file stays small.
//! - **The settings file.** With `ServerConfig::ui_settings_file` they are kept in that
//!   file, read when the server binds and rewritten whole through a temporary file on
//!   every change, user-only like the device file. A file that can't be read or parsed is
//!   logged and set aside: the server starts with no settings, and the next change
//!   replaces it. Without a file they live in memory and are gone when the server stops.
//! - **Not collection state.** Nothing here touches the engine: a change has no `rev`, no
//!   undo and no event, and outlives switching collections.

use crate::devices::create_private;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The settings file's format version.
const FILE_VERSION: u64 = 1;

/// The longest key accepted, in bytes (every key character is ASCII).
pub(crate) const MAX_KEY_BYTES: usize = 64;

/// The largest value accepted, in bytes of compact JSON: 64 KiB.
pub(crate) const MAX_VALUE_BYTES: usize = 64 * 1024;

/// The most keys kept.
pub(crate) const MAX_KEYS: usize = 64;

/// Why a `ui_settings` call was refused: the field and the reason, for
/// `invalid_argument`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    pub(crate) field: &'static str,
    pub(crate) problem: String,
}

/// Why `set` failed.
#[derive(Debug)]
pub(crate) enum SetError {
    /// The key or value was refused; nothing changed.
    Refused(Refused),
    /// The value is kept in memory, but the file could not be written.
    Unsaved(io::Error),
}

/// The UI settings, and the file that keeps them.
#[derive(Debug)]
pub(crate) struct UiSettings {
    file: Option<PathBuf>,
    settings: Mutex<BTreeMap<String, Value>>,
}

impl UiSettings {
    /// The settings in `file`, or none: a missing file holds none, and one that can't be
    /// read is logged and holds none. Without a file they live in memory.
    pub(crate) fn load(file: Option<PathBuf>) -> UiSettings {
        let settings = match file.as_deref() {
            Some(path) => read_file(path).unwrap_or_else(|reason| {
                tracing::warn!(
                    path = %path.display(),
                    %reason,
                    "the UI settings file is unusable, so the UI starts from its defaults; \
                     the next change replaces it"
                );
                BTreeMap::new()
            }),
            None => BTreeMap::new(),
        };
        UiSettings {
            file,
            settings: Mutex::new(settings),
        }
    }

    /// The value under `key`, if there is one.
    pub(crate) fn get(&self, key: &str) -> Result<Option<Value>, Refused> {
        check_key(key)?;
        Ok(self
            .settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned())
    }

    /// Keeps `value` under `key`, or removes the key for `None` or `null`, then rewrites
    /// the file.
    pub(crate) fn set(&self, key: &str, value: Option<Value>) -> Result<(), SetError> {
        check_key(key).map_err(SetError::Refused)?;
        let value = value.filter(|value| !value.is_null());
        let mut settings = self.settings.lock().unwrap_or_else(PoisonError::into_inner);
        match value {
            Some(value) => {
                let bytes = value.to_string().len();
                if bytes > MAX_VALUE_BYTES {
                    return Err(SetError::Refused(Refused {
                        field: "value",
                        problem: format!(
                            "the value is {bytes} bytes as JSON, over the {MAX_VALUE_BYTES} \
                             allowed"
                        ),
                    }));
                }
                if !settings.contains_key(key) && settings.len() >= MAX_KEYS {
                    return Err(SetError::Refused(Refused {
                        field: "key",
                        problem: format!(
                            "{MAX_KEYS} keys are kept already; remove one with a null value \
                             first"
                        ),
                    }));
                }
                settings.insert(key.to_owned(), value);
            }
            None => {
                if settings.remove(key).is_none() {
                    return Ok(());
                }
            }
        }
        match &self.file {
            Some(path) => write_file(path, &settings).map_err(SetError::Unsaved),
            None => Ok(()),
        }
    }
}

/// Checks a key: 1 to [`MAX_KEY_BYTES`] of `a`-`z`, `0`-`9`, `.`, `_` and `-`.
pub(crate) fn check_key(key: &str) -> Result<(), Refused> {
    let refused = |problem: &str| {
        Err(Refused {
            field: "key",
            problem: problem.to_owned(),
        })
    };
    if key.is_empty() {
        return refused("the key is empty");
    }
    if key.len() > MAX_KEY_BYTES {
        return refused("the key is over 64 characters");
    }
    let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b);
    if !key.bytes().all(allowed) {
        return refused("a key holds only a-z, 0-9, '.', '_' and '-'");
    }
    Ok(())
}

/// Reads the settings file: a missing one holds no settings.
fn read_file(path: &Path) -> Result<BTreeMap<String, Value>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.to_string()),
    };
    parse_file(&text)
}

fn parse_file(text: &str) -> Result<BTreeMap<String, Value>, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if value.get("version").and_then(Value::as_u64) != Some(FILE_VERSION) {
        return Err(format!(
            "it is not a version {FILE_VERSION} UI settings file"
        ));
    }
    let entries = value
        .get("settings")
        .and_then(Value::as_object)
        .ok_or("it has no `settings` object")?;
    // A key the server would now refuse, or a null, is dropped rather than kept.
    Ok(entries
        .iter()
        .filter(|(key, value)| check_key(key).is_ok() && !value.is_null())
        .take(MAX_KEYS)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

fn render_file(settings: &BTreeMap<String, Value>) -> String {
    let entries: Map<String, Value> = settings
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut text = json!({"version": FILE_VERSION, "settings": entries}).to_string();
    text.push('\n');
    text
}

/// Writes the settings file whole: to `<path>.tmp`, created user-only, synced, then
/// renamed over `path`, so a crash leaves the old file or the new one. The file's folder
/// is created first: the data folder may not exist yet on a first start.
fn write_file(path: &Path, settings: &BTreeMap<String, Value>) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A temporary file left by a crash is ours to replace.
    match fs::remove_file(&tmp) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let written = create_private(&tmp).and_then(|mut file| {
        file.write_all(render_file(settings).as_bytes())?;
        file.sync_all()
    });
    let renamed = written.and_then(|()| fs::rename(&tmp, path));
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

#[cfg(test)]
mod tests {
    use super::{MAX_KEYS, MAX_VALUE_BYTES, Refused, SetError, UiSettings, check_key};
    use serde_json::{Value, json};
    use std::fs;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jstream-ui-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn refused(result: Result<(), SetError>) -> Refused {
        match result {
            Err(SetError::Refused(refused)) => refused,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn keys_are_checked() {
        for good in ["workspace.dock", "a", "mixer-orientation", "x_1.y-2"] {
            assert_eq!(check_key(good), Ok(()), "{good}");
        }
        let long = "a".repeat(65);
        for bad in ["", "Workspace", "a b", "dock/1", "é", long.as_str()] {
            let refusal = check_key(bad).expect_err(bad);
            assert_eq!(refusal.field, "key", "{bad}");
        }
        assert_eq!(check_key(&"a".repeat(64)), Ok(()));
    }

    #[test]
    fn a_value_is_kept_read_back_and_removed() {
        let settings = UiSettings::load(None);
        assert_eq!(settings.get("workspace.dock").expect("a key"), None);
        let dock = json!({"current": "Live", "zones": [1, 2]});
        settings
            .set("workspace.dock", Some(dock.clone()))
            .expect("kept");
        assert_eq!(settings.get("workspace.dock").expect("a key"), Some(dock));
        settings
            .set("workspace.dock", Some(Value::Null))
            .expect("removed");
        assert_eq!(settings.get("workspace.dock").expect("a key"), None);
        // Removing a key that isn't there succeeds.
        settings
            .set("workspace.dock", None)
            .expect("nothing to remove");
        assert_eq!(
            settings.get("Bad Key").expect_err("refused").field,
            "key",
            "a get checks the key too"
        );
    }

    #[test]
    fn sizes_are_capped() {
        let settings = UiSettings::load(None);
        let big = Value::String("x".repeat(MAX_VALUE_BYTES));
        let refusal = refused(settings.set("big", Some(big)));
        assert_eq!(refusal.field, "value");
        // Two quotes make the string's JSON two bytes longer than its text.
        let fits = Value::String("x".repeat(MAX_VALUE_BYTES - 2));
        settings.set("big", Some(fits)).expect("exactly the limit");
        for n in 1..MAX_KEYS {
            settings
                .set(&format!("k{n}"), Some(json!(n)))
                .expect("under the key limit");
        }
        let refusal = refused(settings.set("one-more", Some(json!(true))));
        assert_eq!(refusal.field, "key");
        // An existing key can still change, and removing one makes room.
        settings
            .set("k1", Some(json!("changed")))
            .expect("replaced");
        settings.set("k1", None).expect("removed");
        settings
            .set("one-more", Some(json!(true)))
            .expect("room again");
    }

    #[test]
    fn the_file_keeps_settings_across_restarts() {
        let dir = scratch("restart");
        // The data folder may not exist yet: the first change creates it.
        let file = dir.join("nested").join("ui-settings.json");
        let first = UiSettings::load(Some(file.clone()));
        let dock = json!({"current": "My layout 1"});
        first
            .set("workspace.dock", Some(dock.clone()))
            .expect("kept");
        first.set("other", Some(json!([1, 2, 3]))).expect("kept");
        first.set("other", None).expect("removed");
        assert!(
            !file.with_extension("json.tmp").exists(),
            "no temporary file is left"
        );
        let second = UiSettings::load(Some(file.clone()));
        assert_eq!(second.get("workspace.dock").expect("a key"), Some(dock));
        assert_eq!(second.get("other").expect("a key"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file).expect("the file").permissions().mode();
            assert_eq!(mode & 0o077, 0, "user-only: {mode:o}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_starts_empty_and_is_replaced() {
        let dir = scratch("broken");
        fs::create_dir_all(&dir).expect("a scratch directory");
        let file = dir.join("ui-settings.json");
        for broken in [
            "not json",
            r#"{"version": 2, "settings": {}}"#,
            r#"{"version": 1}"#,
        ] {
            fs::write(&file, broken).expect("written");
            let settings = UiSettings::load(Some(file.clone()));
            assert_eq!(
                settings.get("workspace.dock").expect("a key"),
                None,
                "{broken}"
            );
        }
        let settings = UiSettings::load(Some(file.clone()));
        settings
            .set("workspace.dock", Some(json!(1)))
            .expect("kept");
        let again = UiSettings::load(Some(file.clone()));
        assert_eq!(again.get("workspace.dock").expect("a key"), Some(json!(1)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_drops_keys_it_would_refuse() {
        let dir = scratch("refused-keys");
        fs::create_dir_all(&dir).expect("a scratch directory");
        let file = dir.join("ui-settings.json");
        fs::write(
            &file,
            r#"{"version": 1, "settings": {"ok": 1, "Not OK": 2, "gone": null}}"#,
        )
        .expect("written");
        let settings = UiSettings::load(Some(file));
        assert_eq!(settings.get("ok").expect("a key"), Some(json!(1)));
        assert_eq!(settings.get("gone").expect("a key"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
