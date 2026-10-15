//! The virtual camera's end of the pipeline: the render thread publishes each frame it
//! composites to the platform's [`VcamSink`], with no encoder, mixer or output thread.
//!
//! The engine owns one sink for its whole life ([`VcamSlot`]). A start takes it out of
//! the slot and starts it, the render thread publishes to it, and when the render thread
//! ends (a stop, a failure or a panic) the sink is stopped and put back, so the next
//! start finds it. A start that fails before the render thread runs puts it back too.

use std::sync::{Arc, Mutex};

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{VcamFormat, VcamSink, VideoSurface};

use crate::lock;

/// Where the engine keeps its virtual-camera sink between starts. Empty when the
/// platform has none, or while an output is using it.
pub(crate) type VcamSlot = Arc<Mutex<Option<Box<dyn VcamSink>>>>;

/// The sink while one output uses it. Dropping it stops the camera and returns the
/// sink to its slot.
pub(crate) struct VcamOut {
    sink: Option<Box<dyn VcamSink>>,
    slot: VcamSlot,
}

impl VcamOut {
    /// Takes `sink`, which goes back to `slot` when this drops.
    pub(crate) fn new(sink: Box<dyn VcamSink>, slot: VcamSlot) -> Self {
        Self {
            sink: Some(sink),
            slot,
        }
    }

    /// Creates the camera so apps can see it.
    pub(crate) fn start(&mut self, format: VcamFormat) -> Result<(), String> {
        let sink = self.sink.as_mut().ok_or("the virtual camera is gone")?;
        sink.start(format)
            .map_err(|err| format!("starting the virtual camera: {err}"))
    }

    /// Publishes one composited RGBA8 frame at `size`, as the BGRA8 the camera offers.
    pub(crate) fn publish(
        &mut self,
        size: FrameSize,
        pts: MediaTime,
        rgba: &[u8],
    ) -> Result<(), String> {
        let sink = self.sink.as_mut().ok_or("the virtual camera is gone")?;
        // `VideoFrame` owns its plane, so each frame needs its own buffer.
        let mut bgra = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        let info = VideoFrameInfo {
            size,
            format: PixelFormat::Bgra8,
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            pts,
        };
        let frame = VideoFrame::new(info, vec![Plane::new(bgra, size.width() as usize * 4)])
            .map_err(|err| format!("a virtual camera frame: {err}"))?;
        sink.publish(VideoSurface::Cpu(frame))
            .map_err(|err| format!("publishing to the virtual camera: {err}"))
    }
}

impl Drop for VcamOut {
    fn drop(&mut self) {
        let Some(mut sink) = self.sink.take() else {
            return;
        };
        if let Err(err) = sink.stop() {
            tracing::error!(error = %err, "stopping the virtual camera failed");
        }
        *lock(&self.slot) = Some(sink);
    }
}
