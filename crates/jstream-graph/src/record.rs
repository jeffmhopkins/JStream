//! The recording output a collection starts with (ADR-0262): a folder output into the
//! engine's own recordings folder, so Record works before anything is set up.

use crate::model::{Chains, Graph, Output};
use jstream_protocol::{BusId, OutputId, OutputKind, RecordContainer};

/// The ID of the recording output a new collection starts with, and that a collection
/// file from before version 24 gains when it has no recording output.
pub const DEFAULT_RECORDING: &str = "recording";

/// The kind of the default recording output: MP4 into the engine's own recordings
/// folder (an empty folder `path`).
pub fn default_recording_kind() -> OutputKind {
    OutputKind::Record {
        path: String::new(),
        container: RecordContainer::Mp4,
        folder: true,
    }
}

impl Graph {
    /// This graph with the default recording output, [`DEFAULT_RECORDING`], when it has
    /// no recording output; else unchanged. The revision stays as it is: it is what a
    /// new collection starts with, not an edit, so there is nothing to undo. It is an
    /// ordinary output, which the user can change or delete.
    #[must_use]
    pub fn with_default_recording(mut self) -> Graph {
        let has_recording = self
            .state
            .outputs
            .values()
            .any(|output| matches!(output.kind, OutputKind::Record { .. }));
        let id = OutputId::new(DEFAULT_RECORDING);
        if !has_recording && !self.state.outputs.contains_key(&id) {
            self.state.outputs.insert(
                id.clone(),
                Output {
                    id,
                    kind: default_recording_kind(),
                    encoder_profile: None,
                    filters: Chains::default(),
                    video_mix: None,
                    audio_bus: BusId::master(),
                },
            );
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_RECORDING, default_recording_kind};
    use crate::Graph;
    use jstream_protocol::{Edit, OutputId, OutputKind, RecordContainer};

    #[test]
    fn a_new_collection_records_into_the_engines_folder() {
        let graph = Graph::new().with_default_recording();
        let output = graph.state().output(&OutputId::new(DEFAULT_RECORDING));
        assert_eq!(
            output.map(|o| o.kind().clone()),
            Some(default_recording_kind())
        );
        assert_eq!(graph.rev(), Graph::new().rev());
    }

    #[test]
    fn a_collection_with_a_recording_gets_no_second() {
        let mut graph = Graph::new();
        let created = graph.apply(
            &[Edit::OutputCreate {
                output_id: OutputId::new("rec"),
                kind: OutputKind::Record {
                    path: "/home/me/show.mp4".into(),
                    container: RecordContainer::Mp4,
                    folder: false,
                },
            }],
            None,
        );
        assert!(created.is_ok(), "{created:?}");
        let before = graph.state().clone();
        assert_eq!(graph.with_default_recording().state(), &before);
    }
}
