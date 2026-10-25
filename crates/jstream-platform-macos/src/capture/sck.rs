//! The ScreenCaptureKit backend: listing from `SCShareableContent`, and an `SCStream`
//! per capture whose frames are copied out as NV12.
//!
//! ScreenCaptureKit is asynchronous: listing, starting and stopping take completion
//! handlers. The control thread waits for each on a channel, with a timeout. Frames
//! arrive on the stream's own serial dispatch queue, in `Output`'s callbacks, which catch
//! panics (R-UNSAFE-005) and only touch the stream's `Shared` state.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use dispatch2::{DispatchQueue, DispatchRetained};
use jstream_platform::mailbox::Mailbox;
use jstream_platform::media::{
    ColorInfo, FrameSize, MediaTime, PixelFormat, VideoFrame, VideoFrameInfo,
};
use jstream_platform::nv12::bgra_to_nv12;
use jstream_platform::{
    Capture, CaptureOptions, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo,
    CursorCapture, PlatformError, VideoSurface,
};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGMainDisplayID, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
    kCGColorSpaceSRGB,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetWidth,
};
use objc2_foundation::{NSArray, NSError, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamErrorDomain, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType, SCWindow,
};

use super::policy::{
    AccessStep, FrameUse, Wanted, access_step, bgra_frame_size, display_id, display_name,
    frame_use, listable_window, pixel_size, resolve, sck_error, screen_recording_denied,
    stop_reason, window_id, window_name,
};
use crate::fourcc::CV_BGRA;
use crate::pixel_buffer::{ReadLocked, presentation_time};
use crate::{MacosVersion, check_floor, timebase};

/// A captured frame, as errors name it.
const FRAME: &str = "a captured frame";

/// How long listing and starting wait for ScreenCaptureKit's answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long dropping a stream waits for it to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// Display and window capture through ScreenCaptureKit (ADR-0063 §3).
///
/// It delivers `VideoSurface::Cpu` NV12 frames in BT.709 limited range, converted from
/// ScreenCaptureKit's sRGB BGRA, with the cursor drawn in unless
/// [`CursorCapture::Hidden`] leaves it out. `pts` is each frame's presentation time on
/// the [`MachClock`](crate::MachClock) time line. A window's frames
/// keep the size it had when the capture started: a resized window is scaled to fit,
/// keeping its aspect ratio.
///
/// Listing and starting need the Screen Recording permission. When it isn't granted, the
/// first call asks macOS to prompt for it, and every call reports an error naming the
/// System Settings pane until it is granted and JStream restarts.
#[derive(Debug)]
pub struct ScreenCaptureKitCapture {
    /// Whether this backend has asked macOS for the permission, which it does once.
    asked: AtomicBool,
}

impl ScreenCaptureKitCapture {
    /// The backend, on a release at or above ADR-0063's floor. Asks for nothing yet.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] below macOS 14.0.
    pub fn new() -> Result<Self, PlatformError> {
        check_floor(MacosVersion::current())?;
        Ok(Self {
            asked: AtomicBool::new(false),
        })
    }

    /// Whether JStream has the Screen Recording permission now. Never prompts.
    pub fn screen_recording_granted() -> bool {
        CGPreflightScreenCaptureAccess()
    }

    /// Checks the permission, asking macOS for it the first time it is missing.
    fn access(&self) -> Result<(), PlatformError> {
        let granted = Self::screen_recording_granted();
        match access_step(granted, self.asked.load(Ordering::Acquire)) {
            AccessStep::Proceed => Ok(()),
            AccessStep::Refuse => Err(screen_recording_denied(false)),
            AccessStep::Ask => {
                // Of two calls racing here, only one asks.
                if self.asked.swap(true, Ordering::AcqRel) {
                    return Err(screen_recording_denied(false));
                }
                if CGRequestScreenCaptureAccess() {
                    Ok(())
                } else {
                    Err(screen_recording_denied(true))
                }
            }
        }
    }
}

impl Capture for ScreenCaptureKitCapture {
    /// The displays, then the on-screen windows of other applications. A display is named
    /// `Display <id>`; a window by its title, else its application's name.
    ///
    /// # Errors
    /// [`PlatformError::Os`] naming the Screen Recording pane when the permission isn't
    /// granted, and when ScreenCaptureKit fails or doesn't answer.
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
        self.access()?;
        let content = shareable_content(true)?;
        let main = CGMainDisplayID();
        let own = i64::from(std::process::id());
        let (displays, windows) = arrays(&content);
        let mut list = Vec::new();
        for display in &displays {
            let id = display_number(&display);
            list.push(CaptureTargetInfo {
                target: CaptureTarget::Display(display_id(id)),
                name: display_name(id),
                size: filter_size(&display_filter(&display)),
                primary: id == main,
            });
        }
        for window in &windows {
            let w = WindowInfo::of(&window);
            let (width, height) = (w.frame.size.width, w.frame.size.height);
            if !listable_window(w.layer, w.pid == Some(own), width, height) {
                continue;
            }
            let Some(name) = window_name(w.title.as_deref(), w.application.as_deref()) else {
                continue;
            };
            list.push(CaptureTargetInfo {
                target: CaptureTarget::Window(window_id(w.id)),
                name,
                size: filter_size(&window_filter(&window)),
                primary: false,
            });
        }
        Ok(list)
    }

    fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
        self.start_with(target, &CaptureOptions::default())
    }

    /// As [`start`](Capture::start). This backend doesn't send the pointer apart from the
    /// frames yet, so [`CursorCapture::Metadata`] draws it in; [`CursorCapture::Hidden`]
    /// turns the configuration's `showsCursor` off.
    fn start_with(
        &self,
        target: &CaptureTarget,
        options: &CaptureOptions,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        let wanted = resolve(target)?;
        self.access()?;
        // Off-screen windows too, so a window on another Space can be captured.
        let content = shareable_content(false)?;
        let (displays, windows) = arrays(&content);
        let (filter, what) = match wanted {
            Wanted::Display(id) => {
                let display = displays
                    .iter()
                    .find(|d| display_number(d) == id)
                    .ok_or_else(|| PlatformError::NotFound {
                        kind: "display",
                        id: display_id(id).as_str().to_owned(),
                    })?;
                (display_filter(&display), format!("display {id}"))
            }
            Wanted::Window(id) => {
                let window = windows
                    .iter()
                    .find(|w| WindowInfo::of(w).id == id)
                    .ok_or_else(|| PlatformError::NotFound {
                        kind: "window",
                        id: window_id(id).as_str().to_owned(),
                    })?;
                (window_filter(&window), format!("window {id}"))
            }
        };
        Ok(Box::new(SckStream::start(
            &filter,
            what,
            shows_cursor(options.cursor),
        )?))
    }
}

/// The content snapshot's displays and windows.
fn arrays(
    content: &SCShareableContent,
) -> (Retained<NSArray<SCDisplay>>, Retained<NSArray<SCWindow>>) {
    // SAFETY: both getters only read the snapshot, which is immutable once delivered.
    unsafe { (content.displays(), content.windows()) }
}

/// A display's `CGDirectDisplayID`.
fn display_number(display: &SCDisplay) -> u32 {
    // SAFETY: the getter only reads the live display object.
    unsafe { display.displayID() }
}

/// What the target list needs from an `SCWindow`.
struct WindowInfo {
    id: u32,
    title: Option<String>,
    application: Option<String>,
    pid: Option<i64>,
    layer: isize,
    frame: objc2_core_foundation::CGRect,
}

impl WindowInfo {
    fn of(window: &SCWindow) -> Self {
        // SAFETY: each getter only reads the live window object, or the application
        // object it returns, which the `Retained` keeps alive while it is read.
        unsafe {
            let app = window.owningApplication();
            Self {
                id: window.windowID(),
                title: window.title().map(|t| t.to_string()),
                application: app.as_ref().map(|a| a.applicationName().to_string()),
                pid: app.as_ref().map(|a| i64::from(a.processID())),
                layer: window.windowLayer(),
                frame: window.frame(),
            }
        }
    }
}

/// A filter for a whole display.
fn display_filter(display: &SCDisplay) -> Retained<SCContentFilter> {
    let none = NSArray::<SCWindow>::new();
    // SAFETY: a fresh allocation, initialised once, with a live display and an array.
    unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), display, &none)
    }
}

/// A filter for one window, wherever it is.
fn window_filter(window: &SCWindow) -> Retained<SCContentFilter> {
    // SAFETY: a fresh allocation, initialised once, with a live window.
    unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window) }
}

/// The filter's content in pixels: its rectangle in points times its scale.
fn filter_size(filter: &SCContentFilter) -> Option<FrameSize> {
    // SAFETY: `infoForFilter` (macOS 14.0, the crate's floor) reads a live filter, and
    // the info's getters only read it.
    let (rect, scale) = unsafe {
        let info = SCShareableContent::infoForFilter(filter);
        (info.contentRect(), info.pointPixelScale())
    };
    pixel_size(rect.size.width, rect.size.height, f64::from(scale))
}

/// An `NSError` from `call` as a [`PlatformError`].
fn ns_error(call: &'static str, error: &NSError) -> PlatformError {
    let (screencapturekit, code, description) = error_parts(error);
    sck_error(call, screencapturekit, code, &description)
}

/// Whether the error is in ScreenCaptureKit's domain, its code and its description.
fn error_parts(error: &NSError) -> (bool, isize, String) {
    // SAFETY: a ScreenCaptureKit constant, set when the framework loads and never written.
    let domain: &NSString = unsafe { SCStreamErrorDomain };
    (
        error.domain().isEqualToString(domain),
        error.code(),
        error.localizedDescription().to_string(),
    )
}

fn no_answer(call: &'static str, message: &str) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: message.into(),
    }
}

/// Waits for a completion handler's result.
fn wait<T>(
    rx: &Receiver<Result<T, PlatformError>>,
    call: &'static str,
    timeout: Duration,
) -> Result<T, PlatformError> {
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            Err(no_answer(call, "ScreenCaptureKit didn't answer in time"))
        }
        Err(RecvTimeoutError::Disconnected) => Err(no_answer(
            call,
            "ScreenCaptureKit released the completion handler without calling it",
        )),
    }
}

/// The shareable displays and windows, leaving out desktop windows (the wallpaper and
/// icons), and windows that aren't on screen when `on_screen_only`.
fn shareable_content(on_screen_only: bool) -> Result<Retained<SCShareableContent>, PlatformError> {
    const CALL: &str = "SCShareableContent getShareableContent";
    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: ScreenCaptureKit passes each as null or an object valid for the
                // call; retaining takes this side's own reference.
                let (content, error) =
                    unsafe { (Retained::retain(content), Retained::retain(error)) };
                match (content, error) {
                    (_, Some(e)) => Err(ns_error(CALL, &e)),
                    (Some(c), None) => Ok(c),
                    (None, None) => Err(no_answer(CALL, "returned neither content nor an error")),
                }
            }))
            .unwrap_or_else(|_| Err(no_answer(CALL, "the completion handler panicked")));
            let _ = tx.try_send(result);
        },
    );
    // SAFETY: the method copies the block and calls it once, on a queue of its own. The
    // content it hands over moves to this thread, which only reads it: the snapshot is
    // immutable once delivered, and retain counts are atomic.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true,
            on_screen_only,
            &handler,
        );
    }
    wait(&rx, CALL, ANSWER_TIMEOUT)
}

/// Runs `call`, which passes the handler it is given to a ScreenCaptureKit method that
/// takes an `NSError` completion handler, and waits for it.
fn completed(
    what: &'static str,
    timeout: Duration,
    call: impl FnOnce(&DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), PlatformError> {
    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(move |error: *mut NSError| {
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: null on success, else an error valid for the call; retaining takes
            // this side's own reference.
            match unsafe { Retained::retain(error) } {
                Some(e) => Err(ns_error(what, &e)),
                None => Ok(()),
            }
        }))
        .unwrap_or_else(|_| Err(no_answer(what, "the completion handler panicked")));
        let _ = tx.try_send(result);
    });
    call(&handler);
    wait(&rx, what, timeout)
}

/// Whether the stream draws the pointer for `cursor`: only [`CursorCapture::Hidden`]
/// leaves it out.
fn shows_cursor(cursor: CursorCapture) -> bool {
    cursor != CursorCapture::Hidden
}

/// The stream's settings for content of `size` pixels, with the pointer drawn in when
/// `cursor` is set.
fn configuration(size: FrameSize, cursor: bool) -> Retained<SCStreamConfiguration> {
    // SAFETY: a fresh configuration, set up before any stream uses it. Each setter takes
    // a plain value, and `kCGColorSpaceSRGB` is a Core Graphics constant, set when the
    // framework loads and never written.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(size.width() as usize);
        config.setHeight(size.height() as usize);
        config.setPixelFormat(CV_BGRA);
        config.setColorSpaceName(kCGColorSpaceSRGB);
        config.setShowsCursor(cursor);
        config.setScalesToFit(true);
        config.setPreservesAspectRatio(true);
        config
    }
}

/// What a stream's callbacks share with the [`CaptureStream`] that reads it.
#[derive(Debug)]
struct Shared {
    /// The newest frame.
    mailbox: Mailbox<VideoSurface>,
    captured: AtomicU64,
    skipped: AtomicU64,
    /// The first failure, after which the stream delivers nothing more.
    failure: OnceLock<PlatformError>,
    /// The display or window, for messages.
    what: String,
}

impl Shared {
    fn fail(&self, e: PlatformError) {
        let _ = self.failure.set(e);
    }

    fn lose(&self, reason: String) {
        self.fail(PlatformError::DeviceLost {
            what: self.what.clone(),
            reason,
        });
    }
}

/// `Output`'s state.
struct Ivars {
    shared: Arc<Shared>,
    /// A frame the reader never took, reused for the next one of the same size. Only the
    /// stream's serial queue locks it, so the lock is never contended.
    spare: Mutex<Option<VideoFrame>>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `Output` doesn't implement
    // `Drop`. Its methods match the protocols' selectors and types.
    #[unsafe(super(NSObject))]
    #[name = "JStreamScreenCaptureOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                self.guarded(|ivars| ivars.on_sample(sample));
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            self.guarded(|ivars| {
                let (screencapturekit, code, description) = error_parts(error);
                ivars
                    .shared
                    .lose(stop_reason(screencapturekit, code, &description));
            });
        }
    }
);

impl Output {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars {
            shared,
            spare: Mutex::new(None),
        });
        // SAFETY: `NSObject`'s `init`, once, on the fresh allocation.
        unsafe { msg_send![super(this), init] }
    }

    /// Runs a callback's body, turning a panic into the stream's failure (R-UNSAFE-005).
    fn guarded(&self, f: impl FnOnce(&Ivars)) {
        let ivars = self.ivars();
        if catch_unwind(AssertUnwindSafe(|| f(ivars))).is_err() {
            ivars.shared.lose("the capture callback panicked".into());
        }
    }
}

impl Ivars {
    fn on_sample(&self, sample: &CMSampleBuffer) {
        if self.shared.failure.get().is_some() {
            return;
        }
        match self.convert(sample) {
            Ok(Some(frame)) => self.publish(frame),
            Ok(None) => {}
            Err(e) => self.shared.fail(e),
        }
    }

    /// The sample's frame as NV12, or `None` when there is nothing new to show.
    fn convert(&self, sample: &CMSampleBuffer) -> Result<Option<VideoFrame>, PlatformError> {
        match frame_use(frame_status(sample)) {
            FrameUse::Deliver => {}
            FrameUse::Skip => return Ok(None),
            FrameUse::Stopped => {
                self.shared
                    .lose("ScreenCaptureKit stopped the stream".into());
                return Ok(None);
            }
        }
        // SAFETY: the sample is live for the callback; the image buffer comes back with
        // its own reference.
        let Some(image) = (unsafe { sample.image_buffer() }) else {
            return Ok(None);
        };
        // SAFETY: the sample is live for the callback.
        let pts = presentation_time(unsafe { sample.presentation_time_stamp() }, FRAME)?;
        self.shared.captured.fetch_add(1, Ordering::Relaxed);
        let locked = ReadLocked::lock(&image, FRAME)?;
        let stride = CVPixelBufferGetBytesPerRow(&image);
        let height = CVPixelBufferGetHeight(&image);
        let Some((w, h)) = bgra_frame_size(
            CVPixelBufferGetPixelFormatType(&image),
            CVPixelBufferGetWidth(&image),
            height,
            stride,
            stride.saturating_mul(height),
        )?
        else {
            return Ok(None);
        };
        let bytes = locked.bytes(stride, height)?;
        let mut out = self.output(w, h)?;
        out.set_pts(pts);
        bgra_to_nv12(bytes, stride, &mut out)?;
        Ok(Some(out))
    }

    /// The frame to convert into: the spare one if it has this size, else a new one.
    fn output(&self, w: u32, h: u32) -> Result<VideoFrame, PlatformError> {
        let size = FrameSize::new(w, h)?;
        let spare = self.spare.lock().ok().and_then(|mut s| s.take());
        match spare {
            Some(f) if f.info().size == size => Ok(f),
            _ => Ok(VideoFrame::zeroed(VideoFrameInfo {
                size,
                format: PixelFormat::Nv12,
                color: ColorInfo::default(),
                pts: MediaTime::zero(timebase()),
            })?),
        }
    }

    fn publish(&self, frame: VideoFrame) {
        if let Some(old) = self.shared.mailbox.put(VideoSurface::Cpu(frame)) {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            if let (VideoSurface::Cpu(old), Ok(mut spare)) = (old, self.spare.lock()) {
                *spare = Some(old);
            }
        }
    }
}

/// The sample's `SCStreamFrameInfoStatus`, when it has one.
fn frame_status(sample: &CMSampleBuffer) -> Option<i64> {
    // SAFETY: the sample is live; `false` only reads the attachments, creating none.
    let attachments = unsafe { sample.sample_attachments_array(false) }?;
    // SAFETY: CMSampleBuffer.h documents the array as one CFDictionary per sample, keyed
    // by CFStrings, with CF values. `SCStreamFrameInfoStatus` is a ScreenCaptureKit
    // constant, set when the framework loads and never written, and an `NSString` is
    // toll-free bridged to a `CFString`.
    let (attachments, key) = unsafe {
        (
            CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(attachments),
            &*ptr::from_ref::<NSString>(SCStreamFrameInfoStatus).cast::<CFString>(),
        )
    };
    attachments
        .get(0)?
        .get(key)?
        .downcast::<CFNumber>()
        .ok()?
        .as_i64()
}

/// A running capture. Dropping it stops the stream.
struct SckStream {
    stream: Retained<SCStream>,
    /// The stream's output and delegate, which it holds weakly.
    output: Retained<Output>,
    /// The serial queue the output's callbacks run on.
    queue: DispatchRetained<DispatchQueue>,
    shared: Arc<Shared>,
}

// SAFETY: the stream, its output and its queue are touched after `start` only by `Drop`,
// on whichever thread drops this. ScreenCaptureKit's stream methods may be called from any
// thread (they complete asynchronously), dispatch queues are thread-safe, and the
// output's state is `Shared` (atomics, a lock-free mailbox and a `OnceLock`) and a mutex.
unsafe impl Send for SckStream {}

impl std::fmt::Debug for SckStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SckStream")
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

impl SckStream {
    fn start(filter: &SCContentFilter, what: String, cursor: bool) -> Result<Self, PlatformError> {
        let size = filter_size(filter).ok_or_else(|| PlatformError::DeviceLost {
            what: what.clone(),
            reason: "it has no area to capture".into(),
        })?;
        let config = configuration(size, cursor);
        let shared = Arc::new(Shared {
            mailbox: Mailbox::new(),
            captured: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            failure: OnceLock::new(),
            what,
        });
        let output = Output::new(Arc::clone(&shared));
        let queue = DispatchQueue::new("org.jstream.screencapturekit", None);
        let delegate: &ProtocolObject<dyn SCStreamDelegate> = ProtocolObject::from_ref(&*output);
        // SAFETY: a fresh allocation, initialised once, with a live filter and
        // configuration. The delegate is `output`, which this value keeps alive for as
        // long as the stream.
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                filter,
                &config,
                Some(delegate),
            )
        };
        let sink: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
        // SAFETY: `output` implements the output method, and `queue` is a serial queue
        // this value keeps alive, so the callbacks run one at a time.
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                sink,
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| ns_error("SCStream addStreamOutput", &e))?;
        let this = Self {
            stream,
            output,
            queue,
            shared,
        };
        completed("SCStream startCapture", ANSWER_TIMEOUT, |handler| {
            // SAFETY: the stream is live and configured; it copies the handler and calls
            // it once.
            unsafe { this.stream.startCaptureWithCompletionHandler(Some(handler)) };
        })?;
        Ok(this)
    }
}

impl CaptureStream for SckStream {
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
            frames_undecodable: 0,
        }
    }
}

impl Drop for SckStream {
    fn drop(&mut self) {
        let stream = &self.stream;
        // A stream that never started answers with an error, which changes nothing.
        let _ = completed("SCStream stopCapture", STOP_TIMEOUT, |handler| {
            // SAFETY: the stream is live; it copies the handler and calls it once.
            unsafe { stream.stopCaptureWithCompletionHandler(Some(handler)) };
        });
        let sink: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*self.output);
        // SAFETY: removes the output `start` added, from the live stream.
        let _ = unsafe { stream.removeStreamOutput_type_error(sink, SCStreamOutputType::Screen) };
        // Wait for any callback already queued, so none runs after `output` is released.
        self.queue.exec_sync(|| {});
    }
}

#[cfg(test)]
mod tests {
    use super::ScreenCaptureKitCapture;

    /// Only `Hidden` turns `showsCursor` off; this backend doesn't send the pointer
    /// apart yet, so `Metadata` draws it in.
    #[test]
    fn only_a_hidden_cursor_is_left_out() {
        use jstream_platform::CursorCapture;
        assert!(super::shows_cursor(CursorCapture::Drawn));
        assert!(super::shows_cursor(CursorCapture::Metadata));
        assert!(!super::shows_cursor(CursorCapture::Hidden));
    }

    #[test]
    fn the_backend_opens_without_asking_for_anything() {
        // Creating it neither lists nor prompts, so it works on a Mac without the
        // permission, such as the CI runner.
        assert!(ScreenCaptureKitCapture::new().is_ok());
        let _ = ScreenCaptureKitCapture::screen_recording_granted();
    }
}

/// Platform tests against a real Mac (R-TEST-004): behind `hw-tests` and `#[ignore]`d,
/// because they need the Screen Recording permission, which a CI runner can't grant. Run
/// them on a Mac with the permission granted to the terminal:
/// `cargo test -p jstream-platform-macos --features hw-tests -- --ignored capture`.
#[cfg(all(test, feature = "hw-tests"))]
mod hw_tests {
    use std::time::{Duration, Instant};

    use jstream_platform::media::PixelFormat;
    use jstream_platform::{Capture, CaptureTarget, MediaClock, VideoSurface};

    use super::ScreenCaptureKitCapture;
    use crate::MachClock;

    #[test]
    #[ignore = "needs the Screen Recording permission"]
    fn capture_lists_displays_and_delivers_recent_nv12_frames() {
        assert!(
            ScreenCaptureKitCapture::screen_recording_granted(),
            "grant the terminal Screen Recording first"
        );
        let capture = ScreenCaptureKitCapture::new().expect("backend");
        let targets = capture.targets().expect("targets");
        let primary = targets
            .iter()
            .find(|t| t.primary)
            .expect("a primary display");
        assert!(matches!(primary.target, CaptureTarget::Display(_)));
        assert!(primary.size.is_some(), "a display's size is known");
        let clock = MachClock::new().expect("clock");
        let mut stream = capture.start(&primary.target).expect("start");
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = loop {
            if let Some(f) = stream.latest().expect("capturing") {
                break f;
            }
            assert!(Instant::now() < deadline, "no frame within 10 s");
            std::thread::sleep(Duration::from_millis(16));
        };
        let VideoSurface::Cpu(frame) = frame else {
            panic!("expected a CPU frame");
        };
        assert_eq!(frame.info().format, PixelFormat::Nv12);
        assert_eq!(frame.info().size.width() % 2, 0);
        let age = clock.now().ticks() - frame.info().pts.ticks();
        assert!((0..10_000_000).contains(&age), "pts {age} ticks old");
        assert!(stream.stats().frames_captured >= 1);
        drop(stream);
        // A second capture starts after the first is dropped.
        let mut again = capture.start(&primary.target).expect("start again");
        let _ = again.latest();
    }

    #[test]
    #[ignore = "needs the Screen Recording permission"]
    fn a_window_that_is_not_listed_is_not_found() {
        let capture = ScreenCaptureKitCapture::new().expect("backend");
        let missing = CaptureTarget::Window(jstream_platform::DeviceId::new("window:4294967295"));
        assert!(matches!(
            capture.start(&missing),
            Err(jstream_platform::PlatformError::NotFound { kind: "window", .. })
        ));
    }

    #[test]
    #[ignore = "needs a Mac where JStream has not been granted Screen Recording"]
    fn a_missing_permission_is_reported_and_asked_for_once() {
        assert!(!ScreenCaptureKitCapture::screen_recording_granted());
        let capture = ScreenCaptureKitCapture::new().expect("backend");
        let first = capture.targets().expect_err("no permission");
        let second = capture.targets().expect_err("still no permission");
        assert_eq!(first, super::screen_recording_denied(true));
        assert_eq!(second, super::screen_recording_denied(false));
    }
}
