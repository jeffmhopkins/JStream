//! The warm budget's file (ADR-0117 §7): how many scenes besides the program the studio
//! keeps warm and the limit on their GPU bytes (ADR-0150), as `studio.set_warm` last
//! set them, kept in the file the host names
//! (`warm-scenes.json` in the data folder, [`Engine::with_warm_file`]). Per machine and
//! never in the collection, so it holds whatever collection is open.
//!
//! [`Engine::with_warm_file`]: crate::Engine::with_warm_file

use jstream_protocol::{MAX_WARM_GPU_MIB, MAX_WARM_SCENES};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct WarmFile {
    version: u32,
    scenes: u32,
    /// Missing in a file from before 0.117: no limit.
    #[serde(default)]
    gpu_mib: u32,
}

/// The warm budget: how many scenes, and the GPU-byte limit in MiB (0 for none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    pub(crate) scenes: u32,
    pub(crate) gpu_mib: u32,
}

/// The budget kept at `path`, or `None` when there is no file. A file that can't be
/// read, parsed or holds a budget past [`MAX_WARM_SCENES`] or [`MAX_WARM_GPU_MIB`] is
/// logged and read as none; the next `studio.set_warm` replaces it.
pub(crate) fn load(path: &Path) -> Option<Budget> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "warm scenes file not read");
            return None;
        }
    };
    match serde_json::from_str::<WarmFile>(&text) {
        Ok(file) if file.scenes <= MAX_WARM_SCENES && file.gpu_mib <= MAX_WARM_GPU_MIB => {
            Some(Budget {
                scenes: file.scenes,
                gpu_mib: file.gpu_mib,
            })
        }
        Ok(file) => {
            tracing::warn!(path = %path.display(), scenes = file.scenes, gpu_mib = file.gpu_mib, "warm scenes file holds too large a budget");
            None
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "warm scenes file not read");
            None
        }
    }
}

/// Saves `budget` at `path`. A failed save is logged: the budget still holds until the
/// engine stops.
pub(crate) fn save(path: &Path, budget: Budget) {
    let mut json = serde_json::to_string_pretty(&WarmFile {
        version: 1,
        scenes: budget.scenes,
        gpu_mib: budget.gpu_mib,
    })
    .expect("invariant: three numbers always serialize");
    json.push('\n');
    if let Err(err) = crate::collection::write_atomically(path, json.as_bytes()) {
        tracing::error!(path = %path.display(), error = %err, "warm scenes not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::{Budget, load, save};

    #[test]
    fn a_saved_budget_loads_and_a_bad_file_reads_as_none() {
        let dir = std::env::temp_dir().join(format!("jstream-warm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("warm-scenes.json");
        assert_eq!(load(&path), None, "no file");
        let budget = Budget {
            scenes: 5,
            gpu_mib: 2048,
        };
        save(&path, budget);
        assert_eq!(load(&path), Some(budget));
        std::fs::write(&path, r#"{"version":1,"scenes":4}"#).expect("written");
        assert_eq!(
            load(&path),
            Some(Budget {
                scenes: 4,
                gpu_mib: 0
            }),
            "a file from before the GPU limit has none"
        );
        std::fs::write(&path, r#"{"version":1,"scenes":99}"#).expect("written");
        assert_eq!(load(&path), None, "past the most");
        std::fs::write(&path, r#"{"version":1,"scenes":3,"gpu_mib":70000}"#).expect("written");
        assert_eq!(load(&path), None, "past the largest limit");
        std::fs::write(&path, "not json").expect("written");
        assert_eq!(load(&path), None, "unparsable");
        std::fs::remove_dir_all(&dir).expect("cleaned up");
    }
}
