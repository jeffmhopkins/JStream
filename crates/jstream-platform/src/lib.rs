//! Platform backend traits: the seam between the engine and the OS. Platform crates
//! (`jstream-platform-windows` first) implement them, and the engine holds the backends as
//! injected trait objects, so engine code never names a platform API. Layer 1: depends
//! only on `jstream-media`, `thiserror` and `wgpu` (for the GPU-frame types).
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-platform/README.md#purpose, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0004-wgpu-with-native-interop.md, docs/adr/0014-media-clock.md, docs/adr/0020-portable-windows-distribution.md

#![forbid(unsafe_code)]

mod audio;
mod backends;
mod capture;
mod clock;
mod codec;
mod error;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
mod gpu;
mod id;
mod surface;
#[cfg(test)]
mod tests;
mod vcam;
mod video_device;

pub use audio::{
    AudioDeviceInfo, AudioDirection, AudioInputHandler, AudioInputTarget, AudioIo,
    AudioOutputHandler, AudioOutputTarget, AudioPacket, AudioPacketFlags, AudioRenderBuffer,
    AudioStream,
};
pub use backends::Backends;
pub use capture::{Capture, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo};
pub use clock::MediaClock;
pub use codec::{
    AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, Codecs, HardwarePreference,
    RateControl, VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
};
pub use error::PlatformError;
pub use gpu::{AdapterId, FrameSync, GpuContext, GpuFrame, GpuInterop};
pub use id::DeviceId;
pub use surface::VideoSurface;
pub use vcam::{VcamFormat, VcamSink};
pub use video_device::{
    CaptureDecoder, CaptureEncoding, CompressedFrame, VideoDeviceFormat, VideoDeviceInfo,
    VideoDevices,
};

/// The media types the traits are written in, re-exported so backends and the engine can
/// name them through one dependency.
pub use jstream_media as media;
/// The GPU types the GPU-frame traits are written in (ADR-0004), re-exported for the same
/// reason, so every crate uses this crate's exact pin.
pub use wgpu;
