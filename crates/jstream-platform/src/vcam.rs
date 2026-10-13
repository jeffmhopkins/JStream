//! The engine side of the virtual camera
//! (docs/adr/0020-portable-windows-distribution.md).

use jstream_media::{FrameSize, PixelFormat, Timebase};

use crate::{PlatformError, VideoSurface};

/// The format the virtual camera offers to apps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VcamFormat {
    /// The frame size.
    pub size: FrameSize,
    /// The pixel format of the frames the engine publishes.
    pub pixel_format: PixelFormat,
    /// The duration of one frame, as a time base: `1/30` for 30 fps.
    pub frame_period: Timebase,
}

/// The engine side of the virtual camera: creates the OS camera for the session and
/// publishes frames from a video mix to it (ADR-0020).
///
/// On Windows the camera itself is a separate COM media source DLL that Frame Server
/// loads; frames cross processes through a shared texture, a shared fence and a
/// versioned control block (spike S2). Registering that DLL is the app's job, not this
/// trait's: [`start`](VcamSink::start) fails with [`PlatformError::Unsupported`] or
/// [`PlatformError::Os`] when the camera is not registered.
///
/// Driven by one thread (so `Send`). Dropping a started sink stops it.
pub trait VcamSink: Send {
    /// Creates the camera so apps can see it, offering `format`.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] when already started,
    /// [`PlatformError::Unsupported`] when the format or the virtual camera itself is not
    /// available, and [`PlatformError::Os`] when the OS refuses.
    fn start(&mut self, format: VcamFormat) -> Result<(), PlatformError>;

    /// Publishes one frame: the newest frame wins, and a consumer that is behind skips to
    /// it. Must not block on the consumer, which may have died (spike S2's DLL reads only
    /// frames whose fence has already completed).
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] when not started, [`PlatformError::Unsupported`]
    /// when the frame doesn't match the started format, and [`PlatformError::Os`] when
    /// the OS fails.
    fn publish(&mut self, frame: VideoSurface) -> Result<(), PlatformError>;

    /// Removes the camera. Does nothing when not started.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the OS fails to remove it.
    fn stop(&mut self) -> Result<(), PlatformError>;

    /// The format the camera was started with, or `None` when stopped.
    fn format(&self) -> Option<VcamFormat>;
}
