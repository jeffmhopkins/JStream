//! An open V4L2 capture: a thread of its own waits on the device, converts or decodes
//! each filled buffer, hands it back to the driver, and puts the frame in the
//! single-slot mailbox [`CaptureStream::latest`] reads (the same mailbox display capture
//! uses). Dropping the stream wakes the thread through an `eventfd` and joins it, which
//! turns the device's stream off and closes it.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use jstream_platform::mailbox::Mailbox;
use jstream_platform::media::{ColorInfo, FrameSize, PixelFormat, VideoFrameInfo};
use jstream_platform::{
    CaptureDecoder, CaptureEncoding, CaptureStats, CaptureStream, CompressedFrame, MediaClock,
    PlatformError, VideoSurface,
};

use super::abi::BUF_FLAG_ERROR;
use super::device::{Filled, Streaming};
use super::format::{pts, raw_frame};
use crate::MonotonicClock;

/// How the frames of the opened format become surfaces.
pub(crate) enum Deliver {
    /// Converted or copied into an engine frame; each buffer plane's rows `strides[i]`
    /// bytes apart.
    Raw {
        encoding: CaptureEncoding,
        strides: Vec<usize>,
    },
    /// Handed to the engine's decoder.
    Decode(Box<dyn CaptureDecoder>),
}

/// What the capture thread shares with the stream.
#[derive(Default)]
struct Shared {
    mailbox: Mailbox<VideoSurface>,
    captured: AtomicU64,
    skipped: AtomicU64,
    undecodable: AtomicU64,
    lost: AtomicBool,
    /// Why, the first time.
    reason: Mutex<Option<PlatformError>>,
}

impl Shared {
    fn fail(&self, error: PlatformError) {
        if let Ok(mut r) = self.reason.lock()
            && r.is_none()
        {
            *r = Some(error);
        }
        self.lost.store(true, Ordering::Release);
    }
}

/// Everything the capture thread owns.
pub(crate) struct Capture {
    pub(crate) streaming: Streaming,
    pub(crate) deliver: Deliver,
    pub(crate) size: FrameSize,
    pub(crate) color: ColorInfo,
}

/// A capture from one V4L2 device.
pub(crate) struct V4l2Stream {
    shared: Arc<Shared>,
    what: String,
    /// Written once to stop the thread.
    wake: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

impl V4l2Stream {
    /// Starts the capture thread on a streaming device.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the wake-up `eventfd` or the thread can't be made.
    pub(crate) fn start(capture: Capture, clock: MonotonicClock) -> Result<Self, PlatformError> {
        let what = capture.streaming.what().to_owned();
        // SAFETY: a plain call; the returned descriptor, if any, is owned below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(PlatformError::Os {
                call: "eventfd",
                code: io::Error::last_os_error()
                    .raw_os_error()
                    .and_then(|c| u32::try_from(c).ok()),
                message: format!("{what}: no wake-up descriptor for its capture thread"),
            });
        }
        // SAFETY: a descriptor this call just made and nothing else owns.
        let wake = unsafe { OwnedFd::from_raw_fd(fd) };
        let shared = Arc::new(Shared::default());
        let thread_shared = Arc::clone(&shared);
        let wake_fd = wake.as_raw_fd();
        let thread = std::thread::Builder::new()
            .name("jstream-v4l2".into())
            .spawn(move || {
                let mut capture = capture;
                let what = capture.streaming.what().to_owned();
                if catch_unwind(AssertUnwindSafe(|| {
                    run(&mut capture, wake_fd, &clock, &thread_shared);
                }))
                .is_err()
                {
                    thread_shared.fail(PlatformError::DeviceLost {
                        what,
                        reason: "its capture thread panicked".into(),
                    });
                }
                // `capture` drops here: the stream turns off and the device closes.
            })
            .map_err(|e| PlatformError::Os {
                call: "std::thread::spawn",
                code: None,
                message: format!("{what}: {e}"),
            })?;
        Ok(Self {
            shared,
            what,
            wake,
            thread: Some(thread),
        })
    }
}

/// The capture thread: waits for a filled buffer or the wake-up, until either says stop.
fn run(capture: &mut Capture, wake: i32, clock: &MonotonicClock, shared: &Shared) {
    let device = capture.streaming.raw();
    loop {
        let mut fds = [
            libc::pollfd {
                fd: device,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two valid `pollfd`s, for the call's duration; no timeout.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            shared.fail(super::device::os_error(
                "poll",
                capture.streaming.what(),
                &e,
            ));
            return;
        }
        if fds[1].revents != 0 {
            return;
        }
        let revents = fds[0].revents;
        if revents == 0 {
            continue;
        }
        let mut got = false;
        loop {
            match capture.streaming.dequeue() {
                Ok(Some(filled)) => {
                    got = true;
                    let index = filled.index;
                    let surface = frame(
                        &filled,
                        &mut capture.deliver,
                        capture.size,
                        capture.color,
                        clock,
                        shared,
                    );
                    if let Err(e) = capture.streaming.queue(index) {
                        shared.fail(e);
                        return;
                    }
                    if let Some(surface) = surface
                        && shared.mailbox.put(surface).is_some()
                    {
                        shared.skipped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    shared.fail(e);
                    return;
                }
            }
        }
        // An error or hang-up with nothing to dequeue: the device stopped (an unplug
        // usually fails the dequeue with ENODEV first).
        if !got && revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            shared.fail(PlatformError::DeviceLost {
                what: capture.streaming.what().to_owned(),
                reason: "the device stopped streaming".into(),
            });
            return;
        }
    }
}

/// The surface for one filled buffer, or `None` to drop it: one the driver marks as
/// corrupt, a raw frame shorter than its format, or a compressed one the decoder
/// rejects (counted).
fn frame(
    filled: &Filled<'_>,
    deliver: &mut Deliver,
    size: FrameSize,
    color: ColorInfo,
    clock: &MonotonicClock,
    shared: &Shared,
) -> Option<VideoSurface> {
    shared.captured.fetch_add(1, Ordering::Relaxed);
    let first = filled.planes.first().copied().unwrap_or_default();
    if filled.flags & BUF_FLAG_ERROR != 0 || first.is_empty() {
        return None;
    }
    let pts = pts(filled.flags, filled.timestamp, clock.now());
    match deliver {
        Deliver::Raw { encoding, strides } => {
            let info = VideoFrameInfo {
                size,
                // Set from the encoding.
                format: PixelFormat::Nv12,
                color,
                pts,
            };
            raw_frame(*encoding, &filled.planes, strides, info)
                .ok()
                .map(VideoSurface::Cpu)
        }
        Deliver::Decode(decoder) => {
            let decoded = decoder.decode(&CompressedFrame {
                encoding: CaptureEncoding::Mjpeg,
                data: first,
                size,
                pts,
            });
            if decoded.is_err() {
                shared.undecodable.fetch_add(1, Ordering::Relaxed);
            }
            decoded.ok()
        }
    }
}

impl CaptureStream for V4l2Stream {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        if let Some(frame) = self.shared.mailbox.take() {
            return Ok(Some(frame));
        }
        if !self.shared.lost.load(Ordering::Acquire) {
            return Ok(None);
        }
        // Never waits: a reason being written is reported generically.
        Err(self
            .shared
            .reason
            .try_lock()
            .ok()
            .and_then(|r| r.clone())
            .unwrap_or_else(|| PlatformError::DeviceLost {
                what: self.what.clone(),
                reason: "the capture stopped".into(),
            }))
    }

    fn stats(&self) -> CaptureStats {
        CaptureStats {
            frames_captured: self.shared.captured.load(Ordering::Relaxed),
            frames_skipped: self.shared.skipped.load(Ordering::Relaxed),
            frames_undecodable: self.shared.undecodable.load(Ordering::Relaxed),
        }
    }
}

impl Drop for V4l2Stream {
    fn drop(&mut self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: writes eight bytes from a live buffer to our own eventfd.
        let _ = unsafe { libc::write(self.wake.as_raw_fd(), one.as_ptr().cast(), one.len()) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use jstream_platform::media::{ColorInfo, FrameSize, PixelFormat};
    use jstream_platform::{
        CaptureDecoder, CaptureEncoding, CompressedFrame, MediaClock, PlatformError, VideoSurface,
    };

    use super::super::abi::{BUF_FLAG_ERROR, BUF_FLAG_TIMESTAMP_MONOTONIC, Timeval};
    use super::super::device::Filled;
    use super::{Deliver, Shared, frame};
    use crate::MonotonicClock;

    /// Decodes nothing: rejects frames starting with 0, and otherwise hands back a black
    /// frame of the size with the frame's `pts`.
    struct Picky;

    impl CaptureDecoder for Picky {
        fn decode(&mut self, f: &CompressedFrame<'_>) -> Result<VideoSurface, PlatformError> {
            if f.data.first() == Some(&0) {
                return Err(PlatformError::Unsupported {
                    what: "a corrupt JPEG".into(),
                });
            }
            let info = jstream_platform::media::VideoFrameInfo {
                size: f.size,
                format: PixelFormat::Nv12,
                color: ColorInfo::default(),
                pts: f.pts,
            };
            Ok(VideoSurface::Cpu(
                jstream_platform::media::VideoFrame::zeroed(info).expect("frame"),
            ))
        }
    }

    fn filled(flags: u32, bytes: &[u8], timestamp: Timeval) -> Filled<'_> {
        Filled {
            index: 0,
            flags,
            timestamp,
            planes: vec![bytes],
        }
    }

    #[test]
    fn buffers_become_frames_or_are_dropped_and_counted() {
        let clock = MonotonicClock::new().expect("clock");
        let shared = Shared::default();
        let size = FrameSize::new(4, 2).expect("size");
        let color = ColorInfo::default();
        let mut raw = Deliver::Raw {
            encoding: CaptureEncoding::Yuy2,
            strides: vec![8],
        };
        // A monotonic timestamp from just now is the frame's time.
        let now = clock.now().ticks();
        let stamp = Timeval {
            tv_sec: now / 10_000_000,
            tv_usec: (now % 10_000_000) / 10,
        };
        let yuy2 = [16u8; 16];
        let out = frame(
            &filled(BUF_FLAG_TIMESTAMP_MONOTONIC, &yuy2, stamp),
            &mut raw,
            size,
            color,
            &clock,
            &shared,
        )
        .expect("a frame");
        assert_eq!(out.info().format, PixelFormat::Nv12);
        assert_eq!(
            out.info().pts.ticks(),
            stamp.tv_sec * 10_000_000 + stamp.tv_usec * 10
        );
        // A corrupt buffer, an empty one and a short one are dropped.
        for (flags, bytes) in [(BUF_FLAG_ERROR, &yuy2[..]), (0, &[][..]), (0, &yuy2[..15])] {
            assert!(
                frame(
                    &filled(flags, bytes, Timeval::default()),
                    &mut raw,
                    size,
                    color,
                    &clock,
                    &shared
                )
                .is_none()
            );
        }
        // Without a monotonic stamp the time is the clock's.
        let before = clock.now();
        let late = frame(
            &filled(0, &yuy2, stamp),
            &mut raw,
            size,
            color,
            &clock,
            &shared,
        )
        .expect("a frame");
        assert!(late.info().pts >= before);

        let mut decode = Deliver::Decode(Box::new(Picky));
        assert!(
            frame(
                &filled(0, &[0, 1], stamp),
                &mut decode,
                size,
                color,
                &clock,
                &shared
            )
            .is_none()
        );
        let decoded = frame(
            &filled(0, &[0xff, 0xd8], stamp),
            &mut decode,
            size,
            color,
            &clock,
            &shared,
        )
        .expect("decoded");
        assert_eq!(decoded.info().size, size);
        assert_eq!(shared.undecodable.load(Ordering::Relaxed), 1);
        assert_eq!(shared.captured.load(Ordering::Relaxed), 7);
    }

    /// A multi-planar NV12M buffer (luma and chroma in separate buffer planes) becomes
    /// one NV12 frame; one missing its chroma plane is dropped.
    #[test]
    fn multi_planar_buffers_become_frames() {
        let clock = MonotonicClock::new().expect("clock");
        let shared = Shared::default();
        let size = FrameSize::new(4, 2).expect("size");
        let mut raw = Deliver::Raw {
            encoding: CaptureEncoding::Raw(PixelFormat::Nv12),
            strides: vec![4, 4],
        };
        let (luma, chroma) = ([1u8; 8], [2u8; 4]);
        let two = Filled {
            index: 1,
            flags: 0,
            timestamp: Timeval::default(),
            planes: vec![&luma, &chroma],
        };
        let out =
            frame(&two, &mut raw, size, ColorInfo::default(), &clock, &shared).expect("a frame");
        let VideoSurface::Cpu(f) = out else {
            panic!("a CPU frame");
        };
        assert_eq!(f.info().format, PixelFormat::Nv12);
        assert_eq!(f.row(1, 0), Some(&[2, 2, 2, 2][..]));
        let one = Filled {
            planes: vec![&luma],
            ..two
        };
        assert!(frame(&one, &mut raw, size, ColorInfo::default(), &clock, &shared).is_none());
        assert_eq!(shared.captured.load(Ordering::Relaxed), 2);
    }
}
