//! Video frames as they cross the platform seam.

use jstream_media::{VideoFrame, VideoFrameInfo};

/// A video frame passed between the engine and a backend: what capture delivers, what an
/// encoder and the virtual-camera sink take, and what a decoder returns.
///
/// Today the only variant is a CPU frame. The real Windows path keeps frames on the GPU
/// (a shared texture and fence, per docs/adr/0004-wgpu-with-native-interop.md); that is
/// added as a new variant together with the `GpuInterop` trait, once spike S1 has a
/// recorded result. The enum is `#[non_exhaustive]` so that addition doesn't break
/// matches in other crates; a consumer that meets a variant it can't handle reports
/// [`PlatformError::Unsupported`](crate::PlatformError::Unsupported).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VideoSurface {
    /// A frame in system memory.
    Cpu(VideoFrame),
}

impl VideoSurface {
    /// The frame's size, pixel format, color and presentation time.
    pub fn info(&self) -> &VideoFrameInfo {
        match self {
            Self::Cpu(frame) => frame.info(),
        }
    }
}

impl From<VideoFrame> for VideoSurface {
    fn from(frame: VideoFrame) -> Self {
        Self::Cpu(frame)
    }
}
