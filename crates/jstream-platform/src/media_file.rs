//! Media files: the seam a media-file source plays a local file through (ADR-0031,
//! docs/adr/0031-source-types-and-decoding-scope.md). The platform demuxes and decodes
//! with its own APIs (the Media Foundation Source Reader on Windows), so the files it
//! plays are the containers and codecs the OS supports.
//!
//! Unlike [`VideoDecoder`](crate::VideoDecoder), which takes packets, a [`MediaDecoder`]
//! owns the whole file: it reads, demuxes and decodes, and hands out frames in
//! presentation order. Reading blocks while a frame decodes, so a source calls it on a
//! thread of its own, never on a real-time thread.

use std::path::Path;

use jstream_media::{FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame};

use crate::{HardwarePreference, PlatformError};

/// What to open a media file for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaDecoderConfig {
    /// The pixel format to decode video to. A backend refuses one it can't produce with
    /// [`PlatformError::Unsupported`].
    pub video_format: PixelFormat,
    /// Hardware or software decoding.
    pub hardware: HardwarePreference,
}

/// What an opened file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaInfo {
    /// The first video stream, or `None` for a file with no video.
    pub video: Option<MediaVideoInfo>,
    /// Whether the file has an audio stream.
    pub audio: bool,
    /// How long the file plays, when the container says.
    pub duration: Option<MediaTime>,
}

/// The video stream a [`MediaDecoder`] decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaVideoInfo {
    /// The size of every decoded frame.
    pub size: FrameSize,
    /// The nominal duration of one frame, when the stream declares a frame rate. Frame
    /// times come from the frames themselves; this is a hint.
    pub frame_period: Option<Timebase>,
}

/// The media-file backend: opens local files for decoding.
///
/// Shared (`Arc<dyn MediaDecoders>`) and called from whichever thread creates a source,
/// so it is `Send + Sync`.
pub trait MediaDecoders: Send + Sync {
    /// Opens the file at `path`, a host path the caller has already resolved, and reads
    /// its streams. Blocks on I/O.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] when the file doesn't exist,
    /// [`PlatformError::Unsupported`] for a container or codec the platform can't decode
    /// or a `video_format` it can't produce, [`PlatformError::MissingComponent`] when the
    /// OS lacks its media stack, and [`PlatformError::Os`] when the OS refuses.
    fn open(
        &self,
        path: &Path,
        config: &MediaDecoderConfig,
    ) -> Result<Box<dyn MediaDecoder>, PlatformError>;
}

/// One open media file, driven by one thread (so `Send`, not `Sync`).
///
/// Frame times are on the file's own timeline, from its start: a frame's `pts` is when
/// it is shown, counting from the start of the file, not the media clock. The caller
/// maps the timeline onto the clock.
pub trait MediaDecoder: Send {
    /// What the file holds.
    fn info(&self) -> &MediaInfo;

    /// The next video frame in presentation order, in the configured format. Blocks
    /// while it is read and decoded. `Ok(None)` at the end of the stream, and for a file
    /// with no video; after the end, only [`seek`](MediaDecoder::seek) brings more.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when reading or decoding fails. A failed read may be
    /// retried; a decoder that can't go on keeps failing.
    fn next_video(&mut self) -> Result<Option<VideoFrame>, PlatformError>;

    /// Moves to `to` on the file's timeline. The next frame read is the one shown at
    /// `to`, or an earlier one: decoding restarts at a key frame, and the backend may
    /// return the frames from it up to `to`. A time past the end gives the end of the
    /// stream.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the file can't seek (a live stream), and
    /// [`PlatformError::Os`] when the OS refuses.
    fn seek(&mut self, to: MediaTime) -> Result<(), PlatformError>;
}
