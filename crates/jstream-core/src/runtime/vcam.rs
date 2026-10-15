//! The virtual camera's end of the pipeline: a running output whose frames go from the
//! program stage's render thread to the platform's [`VcamSink`], with no encoder session
//! and no output thread.
//!
//! The engine owns one sink for its whole life ([`VcamSlot`]). A start takes it out of
//! the slot ([`VcamOut`]) and starts the camera, then hands the render thread a
//! [`VcamTap`] that publishes every program frame to it. The tap and the output share
//! the sink ([`VcamShared`]): a stop takes it out on the control thread, which stops the
//! camera and returns the sink to the slot, so the next start finds it, and the tap
//! leaves on its next tick. Whatever drops the sink instead (a start that fails) does
//! the same.

use std::sync::{Arc, Mutex};

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{VcamFormat, VcamSink, VideoSurface};

use crate::lock;
use crate::runtime::pipeline::FailLine;

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

    fn publish(&mut self, frame: VideoFrame) -> Result<(), String> {
        let sink = self.sink.as_mut().ok_or("the virtual camera is gone")?;
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

/// The sink while its output runs, shared by the render thread's tap and the output's
/// control side. The stop takes it out, on the control thread, so stopping needs no
/// tick from the render thread and never stops the camera there.
pub(crate) type VcamShared = Arc<Mutex<Option<VcamOut>>>;

/// A virtual-camera output as the render thread holds it.
pub(crate) struct VcamTap {
    out: VcamShared,
    /// The size the camera was started at: the canvas.
    canvas: FrameSize,
    /// The output's failure channel: a publish error fails this output alone.
    fail: FailLine,
}

impl VcamTap {
    pub(crate) fn new(out: VcamShared, canvas: FrameSize, fail: FailLine) -> Self {
        Self { out, canvas, fail }
    }

    /// Publishes one program frame, RGBA8 at `size`, as BGRA8 at the canvas size: fitted
    /// inside it and centred, by nearest neighbour, when the sizes differ. `false` once
    /// the tap is done: its output has stopped (the sink has been taken back) or the
    /// camera refused the frame (the output is told it failed, and its stop takes the
    /// sink back).
    pub(crate) fn push(&mut self, size: FrameSize, pts: MediaTime, rgba: &[u8]) -> bool {
        let mut out = lock(&self.out);
        let Some(out) = out.as_mut() else {
            return false;
        };
        let published =
            bgra_frame(size, self.canvas, pts, rgba).and_then(|frame| out.publish(frame));
        match published {
            Ok(()) => true,
            Err(err) => {
                self.fail.report(err);
                false
            }
        }
    }

    /// `false` once the output has stopped; for a tick with no frame to publish yet.
    pub(crate) fn stay(&self) -> bool {
        lock(&self.out).is_some()
    }
}

/// One BGRA8 frame at `canvas` from an RGBA8 frame at `size`. The frame owns its
/// plane, so each needs its own buffer.
fn bgra_frame(
    size: FrameSize,
    canvas: FrameSize,
    pts: MediaTime,
    rgba: &[u8],
) -> Result<VideoFrame, String> {
    let bgra = if size == canvas {
        let mut bgra = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        bgra
    } else {
        fit(size, canvas, rgba)
    };
    let info = VideoFrameInfo {
        size: canvas,
        format: PixelFormat::Bgra8,
        color: ColorInfo {
            space: ColorSpace::Bt709,
            range: ColorRange::Full,
        },
        pts,
    };
    VideoFrame::new(info, vec![Plane::new(bgra, canvas.width() as usize * 4)])
        .map_err(|err| format!("a virtual camera frame: {err}"))
}

/// Scales an RGBA8 frame at `size` into the largest centred rectangle of its aspect
/// ratio inside `canvas`, by nearest neighbour, as BGRA8; the bars are opaque black.
fn fit(size: FrameSize, canvas: FrameSize, rgba: &[u8]) -> Vec<u8> {
    let (sw, sh) = (u64::from(size.width()), u64::from(size.height()));
    let (cw, ch) = (u64::from(canvas.width()), u64::from(canvas.height()));
    // The scaled width and height, rounded down, at least 1 pixel.
    let (w, h) = if sw * ch <= cw * sh {
        ((sw * ch / sh).max(1), ch)
    } else {
        (cw, (sh * cw / sw).max(1))
    };
    let (x0, y0) = ((cw - w) / 2, (ch - h) / 2);
    let mut out = vec![0u8; (cw * ch * 4) as usize];
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    for y in 0..h {
        let sy = y * sh / h;
        let row = ((y0 + y) * cw + x0) * 4;
        for x in 0..w {
            let sx = x * sw / w;
            let s = ((sy * sw + sx) * 4) as usize;
            let d = (row + x * 4) as usize;
            if let (Some(src), Some(dst)) = (rgba.get(s..s + 4), out.get_mut(d..d + 4)) {
                dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::fit;
    use jstream_media::FrameSize;

    fn size(w: u32, h: u32) -> FrameSize {
        FrameSize::new(w, h).expect("size")
    }

    /// A wider frame is letterboxed: black bars above and below, the frame scaled
    /// to the canvas width and swizzled to BGRA.
    #[test]
    fn a_wider_frame_is_letterboxed() {
        let rgba: Vec<u8> = [1, 2, 3, 4].repeat(4);
        let out = fit(size(4, 1), size(4, 4), &rgba);
        let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(px(0, 0), [0, 0, 0, 255]);
        assert_eq!(px(0, 1), [3, 2, 1, 4]);
        assert_eq!(px(3, 1), [3, 2, 1, 4]);
        assert_eq!(px(0, 2), [0, 0, 0, 255]);
    }

    /// A taller frame is pillarboxed, and a same-aspect one fills the canvas.
    #[test]
    fn a_taller_frame_is_pillarboxed_and_a_same_aspect_one_fills() {
        let rgba: Vec<u8> = [9, 8, 7, 255].repeat(2);
        let out = fit(size(1, 2), size(4, 2), &rgba);
        let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(px(0, 0), [0, 0, 0, 255]);
        assert_eq!(px(1, 0), [7, 8, 9, 255]);
        assert_eq!(px(2, 1), [0, 0, 0, 255]);

        let rgba: Vec<u8> = [5, 6, 7, 255].repeat(4);
        let out = fit(size(2, 2), size(4, 4), &rgba);
        assert!(out.chunks_exact(4).all(|px| px == [7, 6, 5, 255]));
    }
}
