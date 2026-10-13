//! Video capture: displays and windows.

use jstream_media::FrameSize;

use crate::{DeviceId, PlatformError, VideoSurface};

/// What to capture.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureTarget {
    /// A whole display (Windows.Graphics.Capture `CreateForMonitor` on Windows).
    Display(DeviceId),
    /// One top-level window.
    Window(DeviceId),
}

/// A capture target as [`Capture::targets`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTargetInfo {
    /// The target, to pass to [`Capture::start`].
    pub target: CaptureTarget,
    /// A human-readable name: the display's name or the window's title.
    pub name: String,
    /// The current size, when the backend knows it without starting a capture.
    pub size: Option<FrameSize>,
    /// Whether this is the primary display. Always `false` for windows.
    pub primary: bool,
}

/// The video-capture backend: lists what can be captured and starts captures.
///
/// Shared (`Arc<dyn Capture>`) and called from the control thread, so it is
/// `Send + Sync`.
pub trait Capture: Send + Sync {
    /// The displays and windows that can be captured now.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the OS refuses to enumerate them.
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError>;

    /// Starts capturing `target`. Frames arrive on the backend's own capture thread and
    /// are read through the returned stream. Dropping the stream stops the capture.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] when the target is gone,
    /// [`PlatformError::Unsupported`] when the backend can't capture that kind of
    /// target, and [`PlatformError::Os`] when the OS refuses.
    fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError>;
}

/// One running capture, read by the render thread.
///
/// The backend's capture thread writes into a single-slot "latest frame" mailbox
/// (docs/ARCHITECTURE.md#threading), and [`latest`](CaptureStream::latest) takes from it,
/// so a slow reader never queues frames: it skips to the newest. The stream moves to the
/// render thread, so it is `Send`. Dropping it stops the capture and releases its
/// resources.
pub trait CaptureStream: Send {
    /// The newest frame delivered since the last call, or `None` if there is none (the
    /// screen did not change; the render loop repeats the last frame). The frame's `pts`
    /// is on the [`MediaClock`](crate::MediaClock). Its size can change between frames,
    /// for example when a window is resized.
    ///
    /// Must not block: the render thread calls it every tick (R-RT-002).
    ///
    /// # Errors
    /// [`PlatformError::DeviceLost`] once the target is gone (a closed window, an
    /// unplugged display). After that the stream delivers nothing more.
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError>;

    /// Counters since the capture started.
    fn stats(&self) -> CaptureStats;
}

/// A capture stream's counters, reported in `stats.tick`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureStats {
    /// Frames the OS delivered to the backend.
    pub frames_captured: u64,
    /// Frames replaced in the mailbox by a newer one before
    /// [`latest`](CaptureStream::latest) took them.
    pub frames_skipped: u64,
}
