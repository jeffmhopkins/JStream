//! The engine side of the virtual camera (`VcamSink`, ADR-0020): it publishes frames into
//! the `jstream-vcam-proto` contract that the `jstream-vcam-windows` media source reads
//! inside Frame Server, and creates the camera with `MFCreateVirtualCamera`.
//!
//! The contract fixes the format: 1920x1080 BGRA at 30 fps, in a ring of shared textures.
//! The checks on formats and frames (`format`) make no Windows calls, so they are tested
//! on every platform; [`WindowsVcam`] is the Windows sink.

// Platform-independent, so it is tested on every platform.
#[cfg(any(windows, test))]
mod format;
#[cfg(windows)]
mod shared;
#[cfg(windows)]
mod sink;

#[cfg(windows)]
pub use sink::WindowsVcam;
