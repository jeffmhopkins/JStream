//! The 3D rendering switches' file (ADR-0170 decision 1): which costly 3D effects the
//! engine draws, as `studio.set_rendering` last set them, kept in the file the host
//! names (`rendering.json` in the data folder, [`Engine::with_rendering_file`]). Per
//! machine and never in the collection, so they hold whatever collection is open, as
//! the warm budget's file does.
//!
//! [`Engine::with_rendering_file`]: crate::Engine::with_rendering_file

use jstream_protocol::RenderFeatures;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct RenderingFile {
    version: u32,
    /// A switch a later engine added, missing here, reads as on.
    #[serde(flatten)]
    features: RenderFeatures,
}

/// The switches kept at `path`, or `None` when there is no file. A file that can't be
/// read or parsed is logged and read as none; the next `studio.set_rendering` replaces
/// it.
pub(crate) fn load(path: &Path) -> Option<RenderFeatures> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "rendering file not read");
            return None;
        }
    };
    match serde_json::from_str::<RenderingFile>(&text) {
        Ok(file) => Some(file.features),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "rendering file not read");
            None
        }
    }
}

/// Saves `features` at `path`. A failed save is logged: the switches still hold until
/// the engine stops.
pub(crate) fn save(path: &Path, features: RenderFeatures) {
    let mut json = serde_json::to_string_pretty(&RenderingFile {
        version: 1,
        features,
    })
    .expect("invariant: a few booleans always serialize");
    json.push('\n');
    if let Err(err) = crate::collection::write_atomically(path, json.as_bytes()) {
        tracing::error!(path = %path.display(), error = %err, "rendering switches not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::{load, save};
    use jstream_protocol::RenderFeatures;

    #[test]
    fn saved_switches_load_and_a_bad_file_reads_as_none() {
        let dir = std::env::temp_dir().join(format!("jstream-rendering-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("rendering.json");
        assert_eq!(load(&path), None, "no file");
        let off = RenderFeatures {
            shadows: false,
            volumetric_fog: false,
            ..RenderFeatures::ALL
        };
        save(&path, off);
        assert_eq!(load(&path), Some(off));
        std::fs::write(&path, r#"{"version":1,"ambient_occlusion":false}"#).expect("written");
        assert_eq!(
            load(&path),
            Some(RenderFeatures {
                ambient_occlusion: false,
                ..RenderFeatures::ALL
            }),
            "a switch the file leaves out is on"
        );
        std::fs::write(&path, "not json").expect("written");
        assert_eq!(load(&path), None, "unparsable");
        std::fs::remove_dir_all(&dir).expect("cleaned up");
    }
}
