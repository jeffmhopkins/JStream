//! The virtual camera's end of the pipeline: a running output whose frames go from the
//! program stage's render thread to the platform's [`VcamSink`], with no encoder
//! session.
//!
//! The engine owns one sink for its whole life ([`VcamSlot`]). A start takes it out of
//! the slot ([`VcamOut`]) and starts the camera, then starts the output's camera thread
//! ([`run_camera`]) and hands the render thread a [`VcamTap`]. The two share nothing but
//! a stop flag and two bounded channels (R-RT-003): each tick the tap copies the program
//! frame into a buffer the camera thread handed back and sends it, dropping and counting
//! the frame when the camera thread is behind. The camera thread converts it to BGRA at
//! the canvas size and publishes it. A stop sets the flag; the camera thread then stops
//! the camera and returns the sink to the slot, so the next start finds it, and the tap
//! leaves on its next tick and goes to the collector. Whatever drops the sink instead (a
//! start that fails) does the same.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// Frames in flight from the render thread to the camera thread. One being published and
/// one waiting: a camera thread further behind than that drops frames rather than
/// adding latency.
const CAMERA_QUEUE: usize = 1;
/// Buffers the camera thread hands back for the render thread to fill: the queued frame,
/// the one being published and one to spare.
const SPARE_BUFFERS: usize = 3;
/// How often a camera thread with no frame coming checks whether its output stopped.
const STOP_POLL: Duration = Duration::from_millis(20);

/// One program frame on its way to the camera: RGBA8 at `size`.
pub(crate) struct CameraFrame {
    size: FrameSize,
    pts: MediaTime,
    rgba: Vec<u8>,
}

/// The ends of a virtual-camera output's channels. `tap` goes to the render thread and
/// the rest to [`run_camera`].
pub(crate) fn camera_channels(
    stop: Arc<AtomicBool>,
    drops: Arc<AtomicU64>,
) -> (VcamTap, Receiver<CameraFrame>, SyncSender<Vec<u8>>) {
    let (frames_tx, frames_rx) = std::sync::mpsc::sync_channel(CAMERA_QUEUE);
    let (spare_tx, spare_rx) = std::sync::mpsc::sync_channel(SPARE_BUFFERS);
    let tap = VcamTap {
        frames: frames_tx,
        spare: spare_rx,
        held: None,
        stop,
        drops,
    };
    (tap, frames_rx, spare_tx)
}

/// A virtual-camera output as the render thread holds it. It never locks, blocks or
/// frees: a leaving tap goes to the collector.
pub(crate) struct VcamTap {
    frames: SyncSender<CameraFrame>,
    /// Buffers the camera thread has published and handed back.
    spare: Receiver<Vec<u8>>,
    /// A buffer whose frame was dropped, kept for the next tick.
    held: Option<Vec<u8>>,
    /// The output's stop flag.
    stop: Arc<AtomicBool>,
    /// The output's drop count: frames the camera thread was too far behind to take.
    drops: Arc<AtomicU64>,
}

impl VcamTap {
    /// Sends one program frame, RGBA8 at `size`, to the camera thread. It reuses a
    /// buffer the camera thread handed back, so it allocates only until buffers
    /// circulate. `false` once the tap is done: its output is stopping, or the camera
    /// thread has ended (a stop, or a failed publish it reported).
    pub(crate) fn push(&mut self, size: FrameSize, pts: MediaTime, rgba: &[u8]) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        let mut buffer = self
            .held
            .take()
            .or_else(|| self.spare.try_recv().ok())
            .unwrap_or_default();
        buffer.clear();
        buffer.extend_from_slice(rgba);
        let frame = CameraFrame {
            size,
            pts,
            rgba: buffer,
        };
        match self.frames.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(frame)) => {
                self.drops.fetch_add(1, Ordering::Relaxed);
                self.held = Some(frame.rgba);
                true
            }
            Err(TrySendError::Disconnected(frame)) => {
                self.held = Some(frame.rgba);
                false
            }
        }
    }

    /// `false` once the output is stopping; for a tick with no frame to send yet.
    pub(crate) fn stay(&self) -> bool {
        !self.stop.load(Ordering::Acquire)
    }
}

/// The body of a virtual-camera output's thread, `jstream-vcam`: converts each frame the
/// tap sends to BGRA8 at `canvas` and publishes it, and hands the buffer back. It ends
/// when `stop` is set, when the tap has gone, or when the camera refuses a frame, which
/// it reports on `fail`; then `out` drops here, which stops the camera and returns the
/// sink to the engine, and `done` drops after it.
pub(crate) fn run_camera(
    mut out: VcamOut,
    canvas: FrameSize,
    frames: Receiver<CameraFrame>,
    spare: SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    fail: FailLine,
    done: Sender<()>,
) {
    while !stop.load(Ordering::Acquire) {
        let frame = match frames.recv_timeout(STOP_POLL) {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let published = bgra_frame(frame.size, canvas, frame.pts, &frame.rgba)
            .and_then(|bgra| out.publish(bgra));
        if let Err(err) = published {
            fail.report(err);
            break;
        }
        // Full only if the tap allocated more than were handed back; this one drops.
        let _ = spare.try_send(frame.rgba);
    }
    drop(out);
    drop(done);
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
