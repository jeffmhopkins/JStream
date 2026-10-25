//! Platform backend traits: the seam between the engine and the OS. Platform crates
//! (`jstream-platform-windows` first) implement them, and the engine holds the backends as
//! injected trait objects, so engine code never names a platform API. Layer 1: depends
//! only on `jstream-media`, `jstream-plugin-proto` (the CLAP plugin hosts' contract),
//! `thiserror`, `wgpu` (for the GPU-frame types) and `crossbeam-utils` (for the
//! mailbox). The [`paths`] module finds the data folder every host keeps its state in
//! (ADR-0066). Three safe, platform-independent helpers are shared by the platform
//! crates: [`mailbox`], the capture callbacks' latest-frame slot, [`nv12`], the CPU
//! RGB-to-NV12 converter, and [`yuv`], the capture devices' raw-frame copies and YUY2
//! conversion. [`draw_cursor`] draws a pointer that a capture sent beside its
//! frames into a CPU frame.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-platform/README.md#purpose, docs/adr/0061-source-types-and-decoding-scope-v2.md, docs/adr/0004-wgpu-with-native-interop.md, docs/adr/0014-media-clock.md, docs/adr/0066-portable-windows-distribution-v2.md

#![forbid(unsafe_code)]

mod audio;
mod backends;
mod capture;
mod clock;
mod codec;
mod cursor;
mod error;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
mod gpu;
mod id;
mod keys;
pub mod mailbox;
mod media_file;
pub mod nv12;
pub mod paths;
pub mod plugin_host;
mod surface;
#[cfg(test)]
mod tests;
mod vcam;
mod video_device;
pub mod yuv;

pub use audio::{
    AudioDeviceInfo, AudioDirection, AudioInputHandler, AudioInputTarget, AudioIo,
    AudioOutputHandler, AudioOutputTarget, AudioPacket, AudioPacketFlags, AudioProcess,
    AudioRenderBuffer, AudioStream,
};
pub use backends::Backends;
pub use capture::{
    Capture, CaptureOptions, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo,
};
pub use clock::MediaClock;
pub use codec::{
    AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, Codecs, HardwarePreference,
    RateControl, VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
};
pub use cursor::{CaptureCursor, CursorCapture, CursorImage, draw_cursor};
pub use error::PlatformError;
pub use gpu::{AdapterId, ContextKey, FrameSync, GpuContext, GpuFrame, GpuInterop};
pub use id::DeviceId;
pub use keys::{OAuthTokenStore, Secret, StreamKeyStore};
pub use media_file::{
    MediaAudioInfo, MediaDecoder, MediaDecoderConfig, MediaDecoders, MediaInfo, MediaVideoInfo,
};
pub use plugin_host::{HostBlock, HostRequest, PluginHost, PluginHosts};
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
