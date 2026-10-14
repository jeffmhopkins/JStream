//! The set of backends a platform crate hands the engine.

use std::sync::Arc;

use crate::{AudioIo, Capture, Codecs, GpuInterop, MediaClock, VcamSink};

/// Everything one platform provides, as the engine receives it: plain data holding the
/// injected trait objects, so the engine never names a platform crate.
///
/// A platform crate builds one (for example `jstream-platform-windows` from WGC, WASAPI,
/// Media Foundation and QPC), and the host app passes it to the engine. Tests pass
/// `fake::backends` (feature `fake`).
pub struct Backends {
    /// The media clock every timestamp is on.
    pub clock: Arc<dyn MediaClock>,
    /// Video capture.
    pub capture: Arc<dyn Capture>,
    /// Audio devices.
    pub audio: Arc<dyn AudioIo>,
    /// Encoders and decoders.
    pub codecs: Arc<dyn Codecs>,
    /// GPU interop: the engine's device on the media adapter and the shared textures
    /// (README › GPU frames).
    pub gpu: Arc<dyn GpuInterop>,
    /// The virtual-camera sink, or `None` when the platform has no virtual camera.
    pub vcam: Option<Box<dyn VcamSink>>,
}
