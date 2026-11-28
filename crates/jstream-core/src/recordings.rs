//! Folder recordings (ADR-0262): where a folder recording output's start records to,
//! and the encoding of the last recording, which a recording output with no encoder
//! profile of its own records with. Both are per machine and never in the collection,
//! so they hold whatever collection is open: the engine's own recordings folder is the
//! one the host names ([`Engine::with_recordings_folder`]), and the last encoding is
//! kept in the file it names (`last-recording.json` in the data folder,
//! [`Engine::with_last_recording_file`]).
//!
//! [`Engine::with_recordings_folder`]: crate::Engine::with_recordings_folder
//! [`Engine::with_last_recording_file`]: crate::Engine::with_last_recording_file

use jstream_protocol::EncoderProfile;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// The engine's folder recording state.
#[derive(Default)]
pub(crate) struct Recordings {
    /// The engine's own recordings folder, which an empty folder `path` names.
    pub(crate) folder: Option<PathBuf>,
    /// Where the last recording's encoding is kept, once the host names it.
    pub(crate) file: Option<PathBuf>,
    /// The last recording's encoding, from the file or the last start.
    pub(crate) last: Option<EncoderProfile>,
}

impl Recordings {
    /// The encoding a recording output with no profile of its own records with: the
    /// last recording's, else the engine's default.
    pub(crate) fn fallback(&self) -> EncoderProfile {
        self.last.unwrap_or(EncoderProfile::DEFAULT)
    }

    /// Remembers that a recording started with `profile`, and returns the file to save
    /// it in when that changed what is kept.
    pub(crate) fn started_with(&mut self, profile: EncoderProfile) -> Option<PathBuf> {
        if self.last == Some(profile) {
            return None;
        }
        self.last = Some(profile);
        self.file.clone()
    }

    /// The folder a folder output with `path` records into: `path`, or the engine's
    /// own folder when it is empty.
    ///
    /// # Errors
    /// The reason, when `path` is empty and the engine has no folder of its own, or
    /// `path` is not absolute on this machine.
    pub(crate) fn folder_for(&self, path: &str) -> Result<PathBuf, String> {
        if path.is_empty() {
            return self.folder.clone().ok_or_else(|| {
                "this engine has no recordings folder of its own: set the output's folder"
                    .to_owned()
            });
        }
        let folder = PathBuf::from(path);
        // The collection's check is the same on every platform, so `/Videos` passes it
        // on Windows and `C:\Videos` on Linux, where it would be a relative name.
        if !folder.is_absolute() {
            return Err(format!(
                "the recording folder `{path}` is not absolute on this machine"
            ));
        }
        Ok(folder)
    }
}

/// The local date and time a recording that starts now is stamped with,
/// `2026-11-28_10-17-30`, in the engine machine's time zone (UTC when it has none it
/// can read).
pub(crate) fn stamp_now() -> String {
    jiff::ZonedDateTime::now()
        .strftime("%Y-%m-%d_%H-%M-%S")
        .to_string()
}

#[derive(Serialize, Deserialize)]
struct LastRecordingFile {
    version: u32,
    /// The last recording's encoding.
    profile: EncoderProfile,
}

/// The encoding kept at `path`, or `None` when there is no file. A file that can't be
/// read or parsed is logged and read as none; the next recording replaces it.
pub(crate) fn load(path: &Path) -> Option<EncoderProfile> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "last recording file not read");
            return None;
        }
    };
    match serde_json::from_str::<LastRecordingFile>(&text) {
        Ok(file) if file.profile.problem().is_none() => Some(file.profile),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "last recording file holds an encoding this engine can't open");
            None
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "last recording file not read");
            None
        }
    }
}

/// Saves `profile` at `path`. A failed save is logged: the encoding still holds until
/// the engine stops.
pub(crate) fn save(path: &Path, profile: EncoderProfile) {
    let mut json = serde_json::to_string_pretty(&LastRecordingFile {
        version: 1,
        profile,
    })
    .expect("invariant: an encoder profile always serializes");
    json.push('\n');
    if let Err(err) = crate::collection::write_atomically(path, json.as_bytes()) {
        tracing::error!(path = %path.display(), error = %err, "last recording's encoding not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::{Recordings, load, save, stamp_now};
    use jstream_protocol::{EncoderProfile, RateControl};
    use std::path::PathBuf;

    fn high() -> EncoderProfile {
        let mut profile = EncoderProfile::DEFAULT;
        profile.video.rate_control = RateControl::Cbr { kbps: 20_000 };
        profile
    }

    #[test]
    fn the_last_encoding_saves_loads_and_a_bad_file_reads_as_none() {
        let dir = std::env::temp_dir().join(format!("jstream-last-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("last-recording.json");
        assert_eq!(load(&path), None, "no file");
        save(&path, high());
        assert_eq!(load(&path), Some(high()));
        std::fs::write(&path, "{").expect("written");
        assert_eq!(load(&path), None, "not JSON");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recording_falls_back_to_the_last_encoding() {
        let mut recordings = Recordings {
            file: Some(PathBuf::from("/data/last-recording.json")),
            ..Recordings::default()
        };
        assert_eq!(recordings.fallback(), EncoderProfile::DEFAULT);
        assert_eq!(
            recordings.started_with(high()),
            Some(PathBuf::from("/data/last-recording.json"))
        );
        assert_eq!(recordings.fallback(), high());
        assert_eq!(
            recordings.started_with(high()),
            None,
            "unchanged: not saved"
        );
    }

    #[test]
    fn an_empty_folder_is_the_engines_own() {
        let mut recordings = Recordings::default();
        assert!(recordings.folder_for("").is_err());
        let own = std::env::temp_dir().join("JStream");
        recordings.folder = Some(own.clone());
        assert_eq!(recordings.folder_for(""), Ok(own));
        let elsewhere = std::env::temp_dir().join("shows");
        let text = elsewhere.to_str().expect("a UTF-8 temp path");
        assert_eq!(recordings.folder_for(text), Ok(elsewhere.clone()));
        assert!(recordings.folder_for("relative").is_err());
    }

    #[test]
    fn the_stamp_is_a_date_and_a_time() {
        let stamp = stamp_now();
        assert_eq!(stamp.len(), "2026-11-28_10-17-30".len(), "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'_', "{stamp}");
    }
}
