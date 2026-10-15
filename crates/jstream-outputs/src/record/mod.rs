//! Recording to a file: [`Recorder`] takes the encoded H.264 and AAC packets an output
//! receives (the same packets the RTMP path is built from) and writes fragmented MP4
//! with `jstream-mux`'s [`Fmp4Muxer`](jstream_mux::fmp4::Fmp4Muxer).
//!
//! The file is written under a temporary name next to the destination (`<name>.part`)
//! and renamed on [`Recorder::finish`], so a file at the destination is always a
//! finished recording. Each fragment (one per video keyframe) is synced to disk when
//! it is complete, so a crash loses at most the fragment being built, and the `.part`
//! file it leaves is readable up to its last complete fragment. A full disk or any
//! other I/O error ends the recording with a typed [`RecordError`] and keeps the
//! `.part` file.
//!
//! ```no_run
//! use jstream_mux::fmp4::Fmp4Config;
//! use jstream_outputs::record::Recorder;
//! # fn run(config: Fmp4Config, packets: &[jstream_media::Packet]) -> Result<(), jstream_outputs::record::RecordError> {
//! let mut recorder = Recorder::create("recording.mp4", config)?;
//! for packet in packets {
//!     recorder.write_packet(packet)?;
//! }
//! let path = recorder.finish()?; // "recording.mp4", renamed from "recording.mp4.part"
//! # let _ = path;
//! # Ok(())
//! # }
//! ```
//!
//! Docs: crates/jstream-outputs/README.md#purpose

mod error;
mod recorder;

pub use error::RecordError;
pub use recorder::{RecordStats, Recorder, temp_path_for};
