//! Video frames as they cross the platform seam.

use jstream_media::{VideoFrame, VideoFrameInfo};

use crate::GpuFrame;

/// A video frame passed between the engine and a backend: what capture delivers, what an
/// encoder and the virtual-camera sink take, and what a decoder returns.
///
/// A frame is either in system memory or in a texture shared with the platform (the GPU
/// path of docs/adr/0004-wgpu-with-native-interop.md). The enum is `#[non_exhaustive]`;
/// a consumer that meets a variant it can't handle reports
/// [`PlatformError::Unsupported`](crate::PlatformError::Unsupported).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VideoSurface {
    /// A frame in system memory.
    Cpu(VideoFrame),
    /// A frame in a texture shared with the platform ([`GpuFrame`]).
    Gpu(GpuFrame),
}

impl VideoSurface {
    /// The frame's size, pixel format, color and presentation time.
    pub fn info(&self) -> &VideoFrameInfo {
        match self {
            Self::Cpu(frame) => frame.info(),
            Self::Gpu(frame) => frame.info(),
        }
    }
}

impl From<GpuFrame> for VideoSurface {
    fn from(frame: GpuFrame) -> Self {
        Self::Gpu(frame)
    }
}

impl From<VideoFrame> for VideoSurface {
    fn from(frame: VideoFrame) -> Self {
        Self::Cpu(frame)
    }
}
