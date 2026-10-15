//! One running device capture: a thread that reads the source reader synchronously and
//! publishes each frame to the stream's single-slot mailbox.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use jstream_platform::media::{ColorInfo, FrameSize, MediaTime, VideoFrameInfo};
use jstream_platform::{
    CaptureDecoder, CaptureEncoding, CaptureStats, CaptureStream, CompressedFrame, MediaClock,
    PlatformError, VideoSurface,
};
use windows::Win32::Media::MediaFoundation::{
    IMFSample, MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
    MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED, MFSampleExtension_DeviceReferenceSystemTime,
};

use super::convert::Layout;
use super::mailbox::Mailbox;
use super::source::{MfStarted, Opened, VIDEO_STREAM, start_error, with_bytes};
use crate::QpcClock;
use crate::com::{Com, os_error};

/// How long `open` waits for the device's first read, which is where a device held by
/// another application refuses.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// State shared between the capture thread and the stream.
struct Shared {
    mailbox: Mailbox<VideoSurface>,
    /// Set once, when the capture ends; `latest` returns it from then on.
    failure: OnceLock<PlatformError>,
    captured: AtomicU64,
    skipped: AtomicU64,
    /// Frames the decoder rejected (dropped, per the `CaptureDecoder` contract). Not in
    /// `CaptureStats` yet, which has no field for it.
    decode_failures: AtomicU64,
    /// Set when the stream is dropped: the thread stops after its current read.
    stop: AtomicBool,
}

impl Shared {
    fn fail(&self, e: PlatformError) {
        let _ = self.failure.set(e);
    }
}

/// How the thread turns a sample into a frame.
pub(crate) enum Deliver {
    /// Copied or converted on the capture thread.
    Raw {
        layout: Layout,
        /// The buffer's row stride.
        stride: usize,
    },
    /// Handed to the engine's decoder.
    Decode(Box<dyn CaptureDecoder>),
}

/// What the capture thread needs, moved into it.
pub(crate) struct Capture {
    pub(crate) opened: Opened,
    pub(crate) deliver: Deliver,
    pub(crate) size: FrameSize,
    /// The selected type's subtype, as `u128`.
    pub(crate) subtype: u128,
    pub(crate) color: ColorInfo,
    /// The device's ID, for `NotFound`.
    pub(crate) id: String,
    /// The device, for `DeviceLost`.
    pub(crate) what: String,
    /// Declared last, so a `Capture` dropped whole releases the MF objects above first.
    pub(crate) mf: MfStarted,
}

/// The capture thread's objects, moved to it.
struct OnThread(Capture);

// SAFETY: the Media Foundation objects inside (source reader, media source, activation
// object) are free-threaded, and `MfStarted` keeps the process MTA alive, so the capture
// thread, which never initialised COM, uses them as an implicit MTA member. They are
// moved to that thread and used only there. The decoder is `Send` by its trait.
unsafe impl Send for OnThread {}

impl OnThread {
    /// Unwraps it on the capture thread. A method, so the spawned closure captures the
    /// whole `Send` wrapper rather than its (non-`Send`) field.
    fn into_inner(self) -> Capture {
        self.0
    }
}

/// A running capture.
pub(crate) struct MfStream {
    shared: Arc<Shared>,
}

impl MfStream {
    /// Starts the capture thread and waits for its first read, so a device another
    /// application holds is refused here (`Busy`) rather than on the render thread.
    pub(crate) fn start(capture: Capture) -> Result<Self, PlatformError> {
        let shared = Arc::new(Shared {
            mailbox: Mailbox::new(),
            failure: OnceLock::new(),
            captured: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            decode_failures: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let clock = QpcClock::new()?;
        let (started, first) = mpsc::sync_channel(1);
        let on_thread = OnThread(capture);
        let thread_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("jstream-video-device".into())
            .spawn(move || {
                let capture = on_thread.into_inner();
                // Not an FFI callback, but a panic (the engine's decoder's included) must
                // still end the capture visibly rather than leave `latest` empty forever.
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run(capture, &thread_shared, &clock, started);
                }));
                if result.is_err() {
                    thread_shared.fail(PlatformError::InvalidState {
                        what: "video capture device",
                        state: "the capture thread panicked",
                    });
                }
            })
            .map_err(|e| PlatformError::Os {
                call: "std::thread::spawn",
                code: None,
                message: e.to_string(),
            })?;
        match first.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) | Err(RecvTimeoutError::Timeout) => Ok(Self { shared }),
            Ok(Err(e)) => Err(e),
            Err(RecvTimeoutError::Disconnected) => {
                Err(shared
                    .failure
                    .get()
                    .cloned()
                    .unwrap_or(PlatformError::InvalidState {
                        what: "video capture device",
                        state: "the capture thread ended before its first read",
                    }))
            }
        }
    }
}

impl CaptureStream for MfStream {
    /// Never blocks: a swap of the mailbox's pointer, and a read of the failure cell.
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        if let Some(e) = self.shared.failure.get() {
            return Err(e.clone());
        }
        Ok(self.shared.mailbox.take())
    }

    fn stats(&self) -> CaptureStats {
        CaptureStats {
            frames_captured: self.shared.captured.load(Ordering::Relaxed),
            frames_skipped: self.shared.skipped.load(Ordering::Relaxed),
        }
    }
}

impl Drop for MfStream {
    /// Doesn't wait: the thread sees the flag after its current read (one frame period)
    /// and then releases the device and drops the decoder on its own.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// Maps a sample's time to the media clock.
///
/// A sample that carries `MFSampleExtension_DeviceReferenceSystemTime` (the QPC time the
/// frame was captured, in 100 ns) is on the clock's time line already. Otherwise the
/// reader's timestamp is anchored to the clock at the first frame and advances with the
/// device's own timestamps from there.
struct Timing<'a> {
    clock: &'a QpcClock,
    offset: Option<i64>,
}

impl Timing<'_> {
    fn pts(&mut self, sample: &IMFSample, timestamp: i64) -> MediaTime {
        let tb = self.clock.timebase();
        // SAFETY: plain attribute read on a live sample.
        if let Ok(t) = unsafe { sample.GetUINT64(&MFSampleExtension_DeviceReferenceSystemTime) }
            && let Ok(t) = i64::try_from(t)
        {
            return MediaTime::new(t, tb);
        }
        let clock = self.clock;
        let offset = *self
            .offset
            .get_or_insert_with(|| clock.now().ticks().saturating_sub(timestamp));
        MediaTime::new(timestamp.saturating_add(offset), tb)
    }
}

fn run(
    capture: Capture,
    shared: &Shared,
    clock: &QpcClock,
    started: mpsc::SyncSender<Result<(), PlatformError>>,
) {
    // Bound first, so they drop last: this thread stays in the MTA and Media Foundation
    // stays started until the reader, the source and the decoder (bound below) are
    // released, at every exit.
    let _com = match Com::init() {
        Ok(com) => com,
        Err(e) => {
            let _ = started.send(Err(e));
            return;
        }
    };
    let _mf = capture.mf;
    let Capture {
        opened,
        mut deliver,
        size,
        subtype,
        color,
        id,
        what,
        ..
    } = capture;
    let lost = |reason: &str| PlatformError::DeviceLost {
        what: what.clone(),
        reason: reason.into(),
    };
    let mut timing = Timing {
        clock,
        offset: None,
    };
    let mut started = Some(started);
    while !shared.stop.load(Ordering::Relaxed) {
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: the out-pointers point to valid locals for the duration of the call.
        let read = unsafe {
            opened.reader.ReadSample(
                VIDEO_STREAM,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
        };
        if let Some(tx) = started.take() {
            let first = match &read {
                Ok(()) => Ok(()),
                Err(e) => Err(start_error("IMFSourceReader::ReadSample", &id, e)),
            };
            let failed = first.is_err();
            let _ = tx.send(first);
            if failed {
                return;
            }
        }
        if let Err(e) = read {
            shared.fail(if e.code() == MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED {
                lost("the device was removed")
            } else {
                os_error("IMFSourceReader::ReadSample", &e)
            });
            return;
        }
        let flag = |f: windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_FLAG| {
            flags & f.0.cast_unsigned() != 0
        };
        if flag(MF_SOURCE_READERF_ERROR) {
            shared.fail(lost("the source reader reported an error"));
            return;
        }
        if flag(MF_SOURCE_READERF_ENDOFSTREAM) {
            shared.fail(lost("the device stopped streaming"));
            return;
        }
        if flag(MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED)
            || (flag(MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED)
                && !opened.current_is(subtype, size))
        {
            // The frames no longer match the format the stream was opened in; the engine
            // reopens it. (A "current type changed" that leaves the subtype and size as
            // they were, such as a new stride or colour attribute, is not a change.)
            shared.fail(lost("the device changed its format"));
            return;
        }
        // A stream tick (a gap) carries no sample.
        let Some(sample) = sample else {
            continue;
        };
        shared.captured.fetch_add(1, Ordering::Relaxed);
        let pts = timing.pts(&sample, timestamp);
        let info = VideoFrameInfo {
            size,
            // Set by the layout, or by the decoder.
            format: jstream_platform::media::PixelFormat::Nv12,
            color,
            pts,
        };
        let surface = match &mut deliver {
            Deliver::Raw { layout, stride } => {
                match with_bytes(&sample, |bytes| layout.frame(bytes, *stride, info)) {
                    Ok(Ok(frame)) => VideoSurface::Cpu(frame),
                    Ok(Err(e)) | Err(e) => {
                        shared.fail(e);
                        return;
                    }
                }
            }
            Deliver::Decode(decoder) => {
                let decoded = with_bytes(&sample, |data| {
                    decoder.decode(&CompressedFrame {
                        encoding: CaptureEncoding::Mjpeg,
                        data,
                        size,
                        pts,
                    })
                });
                match decoded {
                    Ok(Ok(surface)) => surface,
                    Ok(Err(_)) => {
                        // A corrupt or unreadable frame is dropped, and the stream goes on.
                        shared.decode_failures.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    Err(e) => {
                        shared.fail(e);
                        return;
                    }
                }
            }
        };
        if shared.mailbox.put(surface).is_some() {
            shared.skipped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
