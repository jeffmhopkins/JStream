//! The warm budget's file (ADR-0117 §7): how many scenes besides the program the studio
//! keeps warm, as `studio.set_warm` last set it, kept in the file the host names
//! (`warm-scenes.json` in the data folder, [`Engine::with_warm_file`]). Per machine and
//! never in the collection, so it holds whatever collection is open.
//!
//! [`Engine::with_warm_file`]: crate::Engine::with_warm_file

use jstream_protocol::MAX_WARM_SCENES;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct WarmFile {
    version: u32,
    scenes: u32,
}

/// The budget kept at `path`, or `None` when there is no file. A file that can't be
/// read, parsed or holds a budget past [`MAX_WARM_SCENES`] is logged and read as none;
/// the next `studio.set_warm` replaces it.
pub(crate) fn load(path: &Path) -> Option<u32> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "warm scenes file not read");
            return None;
        }
    };
    match serde_json::from_str::<WarmFile>(&text) {
        Ok(file) if file.scenes <= MAX_WARM_SCENES => Some(file.scenes),
        Ok(file) => {
            tracing::warn!(path = %path.display(), scenes = file.scenes, "warm scenes file holds too many scenes");
            None
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "warm scenes file not read");
            None
        }
    }
}

/// Saves `scenes` at `path`. A failed save is logged: the budget still holds until the
/// engine stops.
pub(crate) fn save(path: &Path, scenes: u32) {
    let mut json = serde_json::to_string_pretty(&WarmFile { version: 1, scenes })
        .expect("invariant: two numbers always serialize");
    json.push('\n');
    if let Err(err) = crate::collection::write_atomically(path, json.as_bytes()) {
        tracing::error!(path = %path.display(), error = %err, "warm scenes not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::{load, save};

    #[test]
    fn a_saved_budget_loads_and_a_bad_file_reads_as_none() {
        let dir = std::env::temp_dir().join(format!("jstream-warm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("warm-scenes.json");
        assert_eq!(load(&path), None, "no file");
        save(&path, 5);
        assert_eq!(load(&path), Some(5));
        std::fs::write(&path, r#"{"version":1,"scenes":99}"#).expect("written");
        assert_eq!(load(&path), None, "past the most");
        std::fs::write(&path, "not json").expect("written");
        assert_eq!(load(&path), None, "unparsable");
        std::fs::remove_dir_all(&dir).expect("cleaned up");
    }
}
