//! The VideoToolbox H.264 and HEVC encoder (ADR-0063 §6): a `VTCompressionSession` fed
//! CPU frames, whose output becomes Annex B packets on the media clock.
//!
//! The session is opened with [`vt_encoder_settings`]: hardware by default, Apple's
//! software encoder for `HardwarePreference::Avoid`, real time, no frame reordering (so a
//! packet's `dts` is its `pts`, as RTMP wants), and CBR. The software encoders don't take
//! `ConstantBitRate`; there the session falls back to an average bit rate with a one-second
//! hard limit at the same rate.
//!
//! Each frame is copied into a pixel buffer from the session's own pool and submitted
//! with its `pts`. VideoToolbox calls [`output_callback`] on its own thread for every
//! frame, in decode order; the callback converts the sample to Annex B there
//! ([`annexb_access_unit`]) and sends it down a channel that [`VtEncoder::poll`] drains
//! without blocking. At most [`MAX_IN_FLIGHT`] frames are queued at once; beyond that
//! [`can_accept`](VideoEncoder::can_accept) says no and the engine drops the frame
//! (R-RT-004). The session's maximum frame delay is one less, so the encoder has emitted
//! a frame before the call that submits the next one returns: an encoder that held more
//! (the default is no limit) would wait for input that `can_accept` never lets in. The
//! cost is that `encode` may wait, while three frames are held, for the oldest to be
//! emitted: at most about one frame's encode time, which a real-time hardware encoder
//! rarely needs.
//!
//! GPU frames (IOSurface encode targets) follow once the Metal interop spike lands
//! (ADR-0063 §8).

use std::ffi::{c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use jstream_platform::media::{Codec, Packet, Rounding, Timebase, VideoFrame};
use jstream_platform::{
    CodecInfo, CodecRole, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferIsPlanar, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVReturnSuccess,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTCopyVideoEncoderList, VTEncodeInfoFlags, VTSessionCopyProperty,
    VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ConstantBitRate,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxFrameDelayCount, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTProfileLevel_HEVC_Main10_AutoLevel,
    kVTVideoEncoderList_CodecType, kVTVideoEncoderList_DisplayName,
    kVTVideoEncoderList_IsHardwareAccelerated,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

use crate::annexb::{annexb_access_unit, cm_time_parts, copy_rows, media_time_from_cm};
use crate::codec::{CM_H264, CM_HEVC, VtBitRate, VtEncoderSettings, vt_encoder_settings};
use crate::fourcc::os_status;

/// Frames submitted and not yet returned by the output callback, at most. Enough to keep
/// a hardware encoder busy without frame reordering; more only adds latency.
pub(crate) const MAX_IN_FLIGHT: usize = 4;

/// The session's `MaxFrameDelayCount`: frames the encoder may hold before it must emit
/// one. Below [`MAX_IN_FLIGHT`], so a full queue always drains.
const MAX_FRAME_DELAY: usize = MAX_IN_FLIGHT - 1;

/// `kVTPropertyNotSupportedErr`: the encoder doesn't have this property.
const VT_PROPERTY_NOT_SUPPORTED: i32 = -12900;

/// `noErr`.
const NO_ERR: i32 = 0;

fn check(status: i32, call: &'static str, what: &str) -> Result<(), PlatformError> {
    if status == NO_ERR {
        Ok(())
    } else {
        Err(os_status(call, status, what))
    }
}

/// The codec's name in messages and [`CodecInfo`].
fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::Hevc => "HEVC",
        _ => "H.264",
    }
}

/// What the output callback hands to [`VtEncoder::poll`].
#[derive(Debug)]
enum Output {
    /// One encoded frame, already Annex B.
    Frame {
        keyframe: bool,
        /// Core Media value and timescale.
        pts: (i64, i32),
        /// `None` when the sample has no decode time.
        dts: Option<(i64, i32)>,
        data: Vec<u8>,
    },
    /// VideoToolbox dropped the frame.
    Dropped,
    /// Encoding the frame failed.
    Failed(PlatformError),
}

/// What the output callback reaches through its `refcon`. Owned by [`VtEncoder`], boxed so
/// its address is fixed, and dropped only after the session is torn down.
struct CallbackContext {
    codec: Codec,
    sender: Sender<Output>,
    in_flight: AtomicUsize,
}

impl CallbackContext {
    /// One fewer frame in flight, never below zero.
    fn release(&self) {
        let _ = self
            .in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }
}

/// A `VTCompressionSession` that is completed and invalidated when dropped, so no callback
/// runs after it.
struct Session(CFRetained<VTCompressionSession>);

// SAFETY: a compression session has no thread affinity: it needs no run loop and is not
// tied to the thread that created it, and VideoToolbox runs its callbacks on its own
// threads. `Session` is not `Sync`, so only the thread that owns the encoder calls it.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session is live. Completing every pending frame before invalidating
        // means no output callback is pending when the context it uses is freed after us.
        unsafe {
            let _ = self.0.complete_frames(kCMTimeInvalid);
            self.0.invalidate();
        }
    }
}

/// Sets one session property, of the type `key` documents.
fn set_property(
    session: &VTCompressionSession,
    key: &CFString,
    value: &CFType,
    what: &str,
) -> Result<(), PlatformError> {
    // SAFETY: the session, key and value are live CF objects; each caller passes the value
    // type VTCompressionProperties.h documents for its key.
    let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    check(status, "VTSessionSetProperty", what)
}

/// The session's `UsingHardwareAcceleratedVideoEncoder` property, `false` when it can't be
/// read.
fn uses_hardware(session: &VTCompressionSession) -> bool {
    let mut value: *const CFType = ptr::null();
    // SAFETY: the session and the constant key are live, and `value` is a valid out
    // pointer for the CFTypeRef VideoToolbox writes, +1 retained, on success.
    let status = unsafe {
        VTSessionCopyProperty(
            session,
            kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
            None,
            ptr::from_mut(&mut value).cast(),
        )
    };
    if status != NO_ERR {
        return false;
    }
    NonNull::new(value.cast_mut())
        // SAFETY: a Copy function returned it, so this takes over its +1 reference.
        .map(|p| unsafe { CFRetained::from_raw(p) })
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .is_some_and(|b| b.as_bool())
}

/// A pixel buffer locked for writing, unlocked when dropped.
struct LockedPixelBuffer<'a>(&'a CVPixelBuffer);

impl<'a> LockedPixelBuffer<'a> {
    fn lock(buffer: &'a CVPixelBuffer) -> Result<Self, PlatformError> {
        // SAFETY: the buffer is live; a lock with no flags allows writing.
        let status =
            unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::empty()) };
        check(
            status,
            "CVPixelBufferLockBaseAddress",
            "locking an input frame",
        )?;
        Ok(Self(buffer))
    }

    /// Plane `index`'s bytes (the whole buffer for a packed format) and its stride.
    fn plane_mut(&mut self, index: usize) -> Option<(&mut [u8], usize)> {
        let buffer = self.0;
        let (base, stride, rows) = if CVPixelBufferIsPlanar(buffer) {
            (
                CVPixelBufferGetBaseAddressOfPlane(buffer, index),
                CVPixelBufferGetBytesPerRowOfPlane(buffer, index),
                CVPixelBufferGetHeightOfPlane(buffer, index),
            )
        } else if index == 0 {
            (
                CVPixelBufferGetBaseAddress(buffer),
                CVPixelBufferGetBytesPerRow(buffer),
                CVPixelBufferGetHeight(buffer),
            )
        } else {
            return None;
        };
        let base = NonNull::new(base.cast::<u8>())?;
        let len = stride.checked_mul(rows)?;
        // SAFETY: while the buffer is locked, Core Video's base address for the plane
        // points at `rows` rows of `stride` bytes it owns and lets us write. The borrow is
        // tied to `&mut self`, so it ends before the unlock and nothing else aliases it.
        Some((
            unsafe { slice::from_raw_parts_mut(base.as_ptr(), len) },
            stride,
        ))
    }
}

impl Drop for LockedPixelBuffer<'_> {
    fn drop(&mut self) {
        // SAFETY: this buffer was locked with the same (empty) flags in `lock`.
        let _ = unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::empty()) };
    }
}

/// Copies `frame`'s planes into `buffer`.
fn copy_frame(frame: &VideoFrame, buffer: &CVPixelBuffer) -> Result<(), PlatformError> {
    let info = frame.info();
    let mut locked = LockedPixelBuffer::lock(buffer)?;
    for (index, plane) in frame.planes().iter().enumerate() {
        let Some(geometry) = info.plane(index)? else {
            break;
        };
        let (dst, dst_stride) =
            locked
                .plane_mut(index)
                .ok_or_else(|| PlatformError::Unsupported {
                    what: format!(
                        "plane {index} of a {:?} frame in a VideoToolbox pixel buffer",
                        info.format
                    ),
                })?;
        copy_rows(
            plane.data(),
            plane.stride(),
            dst,
            dst_stride,
            geometry.row_bytes,
            usize::try_from(geometry.height).unwrap_or(usize::MAX),
        )?;
    }
    Ok(())
}

/// The function that returns parameter set `index` of a format description.
type ParameterSetAt = unsafe extern "C-unwind" fn(
    &CMFormatDescription,
    usize,
    *mut *const u8,
    *mut usize,
    *mut usize,
    *mut c_int,
) -> i32;

/// The format description's parameter sets (SPS and PPS, and VPS for HEVC) and its NAL
/// unit length size. The slices borrow `desc`'s memory.
fn parameter_sets(
    codec: Codec,
    desc: &CMFormatDescription,
) -> Result<(Vec<&[u8]>, usize), PlatformError> {
    let get: ParameterSetAt = match codec {
        Codec::Hevc => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
        _ => CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    };
    let call = "CMVideoFormatDescriptionGetParameterSetAtIndex";
    let mut count = 0usize;
    let mut nal_length: c_int = 0;
    // SAFETY: `desc` is live; Core Media allows null set and size pointers, and `count`
    // and `nal_length` are valid out pointers.
    let status = unsafe {
        get(
            desc,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut count,
            &mut nal_length,
        )
    };
    check(status, call, "reading the encoder's parameter sets")?;
    let mut sets = Vec::with_capacity(count);
    for index in 0..count {
        let mut set: *const u8 = ptr::null();
        let mut size = 0usize;
        // SAFETY: as above, with valid out pointers for the set and its size.
        let status = unsafe {
            get(
                desc,
                index,
                &mut set,
                &mut size,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        check(status, call, "reading a parameter set")?;
        if set.is_null() || size == 0 {
            continue;
        }
        // SAFETY: Core Media returns a pointer to `size` bytes inside the description,
        // valid while it is retained; the returned slice borrows `desc` for that long.
        sets.push(unsafe { slice::from_raw_parts(set, size) });
    }
    let nal_length = usize::try_from(nal_length).unwrap_or(0);
    Ok((sets, nal_length))
}

/// Whether the sample is a sync sample (a keyframe): it has no `NotSync` attachment set
/// to true.
fn is_keyframe(sample: &CMSampleBuffer) -> bool {
    // SAFETY: the sample is live; `false` only reads the attachments, creating none.
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        return true;
    };
    // SAFETY: CMSampleBuffer.h documents the array as one CFDictionary per sample, keyed
    // by CFStrings, with CF values.
    let attachments = unsafe {
        CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(attachments)
    };
    let Some(first) = attachments.get(0) else {
        return true;
    };
    // SAFETY: a Core Media constant, set before any code runs and never written.
    let not_sync = unsafe { kCMSampleAttachmentKey_NotSync };
    !first
        .get(not_sync)
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .is_some_and(|b| b.as_bool())
}

/// A valid Core Media time's value and timescale.
fn cm_parts(time: CMTime) -> Option<(i64, i32)> {
    (time.flags.contains(CMTimeFlags::Valid) && time.timescale > 0)
        .then_some((time.value, time.timescale))
}

/// One encoded sample as an [`Output::Frame`].
fn encoded(codec: Codec, sample: &CMSampleBuffer) -> Result<Output, PlatformError> {
    let missing = |what: &str| PlatformError::Os {
        call: "VTCompressionOutputCallback",
        code: None,
        message: format!("an encoded frame without {what}"),
    };
    // SAFETY: the sample is live for the callback; these getters only read it, and the
    // block buffer and format description come back retained.
    let (pts, dts, block, desc) = unsafe {
        (
            sample.presentation_time_stamp(),
            sample.decode_time_stamp(),
            sample.data_buffer(),
            sample.format_description(),
        )
    };
    let pts = cm_parts(pts).ok_or_else(|| missing("a presentation time"))?;
    let block = block.ok_or_else(|| missing("data"))?;
    let desc = desc.ok_or_else(|| missing("a format description"))?;
    // SAFETY: the block buffer is live.
    let len = unsafe { block.data_length() };
    let mut data = vec![0u8; len];
    if len > 0 {
        // SAFETY: `data` is `len` writable bytes, the block buffer's whole length.
        let status =
            unsafe { block.copy_data_bytes(0, len, NonNull::from(data.as_mut_slice()).cast()) };
        check(
            status,
            "CMBlockBufferCopyDataBytes",
            "reading an encoded frame",
        )?;
    }
    let keyframe = is_keyframe(sample);
    let (sets, nal_length) = parameter_sets(codec, &desc)?;
    let sets: &[&[u8]] = if keyframe { &sets } else { &[] };
    let mut au = Vec::with_capacity(len + sets.iter().map(|s| s.len() + 4).sum::<usize>());
    annexb_access_unit(&data, nal_length, sets, &mut au)?;
    Ok(Output::Frame {
        keyframe,
        pts,
        dts: cm_parts(dts),
        data: au,
    })
}

/// What the callback reports for one frame.
fn output(
    codec: Codec,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: Option<&CMSampleBuffer>,
) -> Output {
    if status != NO_ERR {
        return Output::Failed(os_status(
            "VTCompressionOutputCallback",
            status,
            "encoding a frame",
        ));
    }
    match sample {
        Some(sample) if !flags.contains(VTEncodeInfoFlags::FrameDropped) => {
            encoded(codec, sample).unwrap_or_else(Output::Failed)
        }
        _ => Output::Dropped,
    }
}

/// VideoToolbox's output callback, on its own thread, once per submitted frame in decode
/// order. No panic leaves it (R-UNSAFE-005).
unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _source_frame_refcon: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `refcon` is the boxed `CallbackContext` the session was created with,
        // which outlives the session (`VtEncoder` drops the session first).
        let Some(context) = (unsafe { refcon.cast::<CallbackContext>().as_ref() }) else {
            return;
        };
        // SAFETY: VideoToolbox passes a sample buffer valid for this call, or null.
        let sample = unsafe { sample.as_ref() };
        let out = catch_unwind(AssertUnwindSafe(|| {
            output(context.codec, status, flags, sample)
        }))
        .unwrap_or_else(|_| {
            Output::Failed(PlatformError::Os {
                call: "VTCompressionOutputCallback",
                code: None,
                message: "converting an encoded frame panicked".into(),
            })
        });
        context.release();
        // A closed channel means the encoder is being dropped; the frame goes with it.
        let _ = context.sender.send(out);
    }));
}

fn number_i64(value: i64) -> CFRetained<CFNumber> {
    CFNumber::new_i64(value)
}

/// `kVTVideoEncoderSpecification_*` for `settings`.
fn encoder_specification(
    settings: &VtEncoderSettings,
) -> CFRetained<CFDictionary<CFString, CFType>> {
    // SAFETY: VideoToolbox constants, set before any code runs and never written.
    let (enable, require) = unsafe {
        (
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
            kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
        )
    };
    let values: [&CFType; 2] = [
        CFBoolean::new(settings.enable_hardware),
        CFBoolean::new(settings.require_hardware),
    ];
    CFDictionary::from_slices(&[enable, require], &values)
}

/// The pool's pixel buffers: the input format and size, IOSurface-backed.
fn source_attributes(
    config: &VideoEncoderConfig,
    pixel_format: u32,
) -> CFRetained<CFDictionary<CFString, CFType>> {
    // SAFETY: Core Video constants, set before any code runs and never written.
    let keys = unsafe {
        [
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferWidthKey,
            kCVPixelBufferHeightKey,
            kCVPixelBufferIOSurfacePropertiesKey,
        ]
    };
    let format = number_i64(i64::from(pixel_format));
    let width = number_i64(i64::from(config.size.width()));
    let height = number_i64(i64::from(config.size.height()));
    let iosurface = CFDictionary::<CFString, CFType>::empty();
    let values: [&CFType; 4] = [&format, &width, &height, &iosurface];
    CFDictionary::from_slices(&keys, &values)
}

/// `[bytes, seconds]` for `kVTCompressionPropertyKey_DataRateLimits`: at most `bps` over
/// one second.
fn data_rate_limit(bps: u64) -> CFRetained<CFArray<CFNumber>> {
    let bytes = number_i64(i64::try_from(bps / 8).unwrap_or(i64::MAX));
    let seconds = CFNumber::new_f64(1.0);
    CFArray::from_retained_objects(&[bytes, seconds])
}

/// Sets the session's rate control, real time, reordering, frame delay, profile and
/// keyframe interval.
fn configure(
    session: &VTCompressionSession,
    config: &VideoEncoderConfig,
    settings: &VtEncoderSettings,
) -> Result<(), PlatformError> {
    // SAFETY: VideoToolbox constants, set before any code runs and never written.
    let (real_time, reordering, delay, profile_key, interval, rate, cbr, average, limits) = unsafe {
        (
            kVTCompressionPropertyKey_RealTime,
            kVTCompressionPropertyKey_AllowFrameReordering,
            kVTCompressionPropertyKey_MaxFrameDelayCount,
            kVTCompressionPropertyKey_ProfileLevel,
            kVTCompressionPropertyKey_MaxKeyFrameInterval,
            kVTCompressionPropertyKey_ExpectedFrameRate,
            kVTCompressionPropertyKey_ConstantBitRate,
            kVTCompressionPropertyKey_AverageBitRate,
            kVTCompressionPropertyKey_DataRateLimits,
        )
    };
    // SAFETY: as above.
    let profile = unsafe {
        match (config.codec, settings.pixel_format) {
            (Codec::Hevc, crate::fourcc::CV_X420) => kVTProfileLevel_HEVC_Main10_AutoLevel,
            (Codec::Hevc, _) => kVTProfileLevel_HEVC_Main_AutoLevel,
            _ => kVTProfileLevel_H264_High_AutoLevel,
        }
    };
    set_property(
        session,
        real_time,
        CFBoolean::new(true),
        "real-time encoding",
    )?;
    set_property(
        session,
        reordering,
        CFBoolean::new(false),
        "no frame reordering",
    )?;
    let max_delay = number_i64(i64::try_from(MAX_FRAME_DELAY).unwrap_or(0));
    let value: &CFType = &max_delay;
    // SAFETY: as in `set_property`: a CFNumber of frames.
    let status = unsafe { VTSessionSetProperty(session, delay, Some(value)) };
    // An encoder without the property keeps its own delay; #627 tracks checking that.
    if status != VT_PROPERTY_NOT_SUPPORTED {
        check(status, "VTSessionSetProperty", "the maximum frame delay")?;
    }
    set_property(session, profile_key, profile, "the profile")?;
    let keyframes = number_i64(i64::from(settings.max_keyframe_interval.max(1)));
    set_property(session, interval, &keyframes, "the keyframe interval")?;
    let period = config.frame_period;
    let fps = CFNumber::new_f64(f64::from(period.den()) / f64::from(period.num()));
    set_property(session, rate, &fps, "the frame rate")?;
    let to_i64 = |bps: u64| i64::try_from(bps).unwrap_or(i64::MAX);
    match settings.bit_rate {
        VtBitRate::Constant(bps) => {
            let target = number_i64(to_i64(bps));
            let value: &CFType = &target;
            // SAFETY: as in `set_property`: a CFNumber of bits per second.
            let status = unsafe { VTSessionSetProperty(session, cbr, Some(value)) };
            if status == VT_PROPERTY_NOT_SUPPORTED {
                // The software encoders have no CBR mode: hold the average to the target
                // with a one-second hard limit at the same rate.
                set_property(session, average, &target, "the average bit rate")?;
                set_property(session, limits, &data_rate_limit(bps), "the bit-rate limit")?;
            } else {
                check(status, "VTSessionSetProperty", "the constant bit rate")?;
            }
        }
        VtBitRate::Average { bps, max_bps } => {
            let target = number_i64(to_i64(bps));
            set_property(session, average, &target, "the average bit rate")?;
            set_property(
                session,
                limits,
                &data_rate_limit(max_bps),
                "the peak bit rate",
            )?;
        }
    }
    // SAFETY: the session is live and configured.
    let status = unsafe { session.prepare_to_encode_frames() };
    check(
        status,
        "VTCompressionSessionPrepareToEncodeFrames",
        "preparing the encoder",
    )
}

/// One VideoToolbox H.264 or HEVC compression session, fed CPU frames.
pub(crate) struct VtEncoder {
    /// Declared before `context`, so it is completed and invalidated before the context
    /// its callback uses is freed.
    session: Session,
    context: Box<CallbackContext>,
    receiver: Receiver<Output>,
    info: CodecInfo,
    config: VideoEncoderConfig,
    /// One frame's duration, as a Core Media time.
    duration: CMTime,
    /// The time base of the first frame's `pts`, which packets are given in.
    timebase: Option<Timebase>,
    force_keyframe: bool,
    finished: bool,
}

impl VtEncoder {
    /// A session for `config`.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a configuration VideoToolbox can't take, or when
    /// `HardwarePreference::Require` finds no hardware encoder, and [`PlatformError::Os`]
    /// when VideoToolbox refuses.
    pub(crate) fn open(config: &VideoEncoderConfig) -> Result<Self, PlatformError> {
        let settings = vt_encoder_settings(config)?;
        let too_big = || PlatformError::Unsupported {
            what: format!(
                "a {}x{} VideoToolbox encoder",
                config.size.width(),
                config.size.height()
            ),
        };
        let width = i32::try_from(config.size.width()).map_err(|_| too_big())?;
        let height = i32::try_from(config.size.height()).map_err(|_| too_big())?;
        let period = config.frame_period;
        let duration = CMTime {
            value: i64::from(period.num()),
            timescale: i32::try_from(period.den()).map_err(|_| PlatformError::Unsupported {
                what: format!("a frame period of {period} on VideoToolbox"),
            })?,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        let (sender, receiver) = mpsc::channel();
        let context = Box::new(CallbackContext {
            codec: config.codec,
            sender,
            in_flight: AtomicUsize::new(0),
        });
        let specification = encoder_specification(&settings);
        let attributes = source_attributes(config, settings.pixel_format);
        let mut raw: *mut VTCompressionSession = ptr::null_mut();
        // SAFETY: both dictionaries map CFString keys to the CF types VideoToolbox and
        // Core Video document for them. `output_callback` has the VTCompressionOutputCallback
        // signature and reads `refcon` only as `&CallbackContext`; `refcon` is the boxed
        // context, which outlives the session (the session is dropped first, here and in
        // `VtEncoder`). `raw` is a valid out pointer.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width,
                height,
                settings.codec_type,
                Some(specification.as_ref()),
                Some(attributes.as_ref()),
                None,
                Some(output_callback),
                ptr::from_ref::<CallbackContext>(&context).cast_mut().cast(),
                NonNull::from(&mut raw),
            )
        };
        let name = codec_name(config.codec);
        if status != NO_ERR && settings.require_hardware {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a hardware {name} encoder on this Mac ({})",
                    os_status("VTCompressionSessionCreate", status, "opening")
                ),
            });
        }
        check(status, "VTCompressionSessionCreate", "opening the encoder")?;
        let raw = NonNull::new(raw).ok_or_else(|| PlatformError::Os {
            call: "VTCompressionSessionCreate",
            code: None,
            message: "returned no session".into(),
        })?;
        // SAFETY: a Create function returned it, so this takes over its +1 reference.
        let session = Session(unsafe { CFRetained::from_raw(raw) });
        configure(&session.0, config, &settings)?;
        let hardware = uses_hardware(&session.0);
        let kind = if hardware { "hardware" } else { "software" };
        Ok(Self {
            session,
            context,
            receiver,
            info: CodecInfo {
                name: format!("VideoToolbox {name} ({kind})"),
                codec: config.codec,
                role: CodecRole::VideoEncoder,
                hardware,
            },
            config: *config,
            duration,
            timebase: None,
            force_keyframe: false,
            finished: false,
        })
    }

    fn refuse_after_finish(&self, what: &'static str) -> Result<(), PlatformError> {
        if self.finished {
            Err(PlatformError::InvalidState {
                what,
                state: "the encoder is finished",
            })
        } else {
            Ok(())
        }
    }

    /// A pixel buffer from the session's pool.
    fn pixel_buffer(&self) -> Result<CFRetained<CVPixelBuffer>, PlatformError> {
        // SAFETY: the session is live.
        let pool =
            unsafe { self.session.0.pixel_buffer_pool() }.ok_or_else(|| PlatformError::Os {
                call: "VTCompressionSessionGetPixelBufferPool",
                code: None,
                message: "the encoder has no pixel buffer pool".into(),
            })?;
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: the pool is live and `raw` is a valid out pointer.
        let status =
            unsafe { CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::from(&mut raw)) };
        if status != kCVReturnSuccess {
            return Err(os_status(
                "CVPixelBufferPoolCreatePixelBuffer",
                status,
                "allocating an input frame",
            ));
        }
        let raw = NonNull::new(raw).ok_or_else(|| PlatformError::Os {
            call: "CVPixelBufferPoolCreatePixelBuffer",
            code: None,
            message: "returned no pixel buffer".into(),
        })?;
        // SAFETY: a Create function returned it, so this takes over its +1 reference.
        Ok(unsafe { CFRetained::from_raw(raw) })
    }

    fn packet(&self, output: Output, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        match output {
            Output::Frame {
                keyframe,
                pts,
                dts,
                data,
            } => {
                let tb = self.timebase.ok_or(PlatformError::InvalidState {
                    what: "VideoToolbox output",
                    state: "no frame was submitted",
                })?;
                let pts_time = media_time_from_cm(pts.0, pts.1, tb)?;
                let dts_time = match dts {
                    Some((value, timescale)) => media_time_from_cm(value, timescale, tb)?,
                    None => pts_time,
                };
                let dts_time = if dts_time > pts_time {
                    pts_time
                } else {
                    dts_time
                };
                out.push(Packet::new(
                    self.config.codec,
                    keyframe,
                    pts_time,
                    dts_time,
                    data,
                )?);
                Ok(())
            }
            Output::Dropped => Ok(()),
            Output::Failed(e) => Err(e),
        }
    }
}

impl VideoEncoder for VtEncoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        !self.finished && self.context.in_flight.load(Ordering::Acquire) < MAX_IN_FLIGHT
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        let name = codec_name(self.config.codec);
        self.refuse_after_finish("VideoToolbox encode")?;
        if !self.can_accept() {
            return Err(PlatformError::Busy {
                what: "VideoToolbox encoder",
            });
        }
        let VideoSurface::Cpu(frame) = frame else {
            return Err(PlatformError::Unsupported {
                what: format!("a GPU frame on the VideoToolbox {name} CPU path"),
            });
        };
        let info = *frame.info();
        if info.size != self.config.size || info.format != self.config.input_format {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a {:?} {}x{} frame (configured for {:?} {}x{})",
                    info.format,
                    info.size.width(),
                    info.size.height(),
                    self.config.input_format,
                    self.config.size.width(),
                    self.config.size.height()
                ),
            });
        }
        let tb = *self.timebase.get_or_insert(info.pts.timebase());
        let pts = info.pts.rescale(tb, Rounding::Nearest)?;
        let (value, timescale) = cm_time_parts(pts).ok_or_else(|| PlatformError::Unsupported {
            what: format!("a frame time in {tb} on VideoToolbox"),
        })?;
        let pts = CMTime {
            value,
            timescale,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        let buffer = self.pixel_buffer()?;
        copy_frame(&frame, &buffer)?;
        let options = self.force_keyframe.then(|| {
            // SAFETY: a VideoToolbox constant, set before any code runs and never written.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            let value: &CFType = CFBoolean::new(true);
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[value])
        });
        self.context.in_flight.fetch_add(1, Ordering::AcqRel);
        // SAFETY: the session and pixel buffer are live, the options dictionary maps the
        // documented key to a CFBoolean, and the frame refcon and info-flags pointers may
        // be null.
        let status = unsafe {
            self.session.0.encode_frame(
                &buffer,
                pts,
                self.duration,
                options.as_deref().map(|d| -> &CFDictionary { d.as_ref() }),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != NO_ERR {
            // VideoToolbox doesn't call back for a frame it refused.
            self.context.release();
            return Err(os_status(
                "VTCompressionSessionEncodeFrame",
                status,
                "submitting a frame",
            ));
        }
        self.force_keyframe = false;
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        while let Ok(output) = self.receiver.try_recv() {
            self.packet(output, out)?;
        }
        Ok(())
    }

    fn request_keyframe(&mut self) -> Result<(), PlatformError> {
        self.refuse_after_finish("VideoToolbox keyframe request")?;
        self.force_keyframe = true;
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        if !self.finished {
            self.finished = true;
            // SAFETY: the session is live; an invalid time completes every pending frame,
            // whose callbacks have all run when this returns.
            let status = unsafe { self.session.0.complete_frames(kCMTimeInvalid) };
            check(
                status,
                "VTCompressionSessionCompleteFrames",
                "draining the encoder",
            )?;
        }
        self.poll(out)
    }
}

/// Every H.264 and HEVC encoder VideoToolbox lists.
///
/// # Errors
/// [`PlatformError::Os`] when VideoToolbox refuses to list them.
pub(crate) fn list_encoders() -> Result<Vec<CodecInfo>, PlatformError> {
    let mut raw: *const CFArray = ptr::null();
    // SAFETY: no options, and `raw` is a valid out pointer for the +1 retained array.
    let status = unsafe { VTCopyVideoEncoderList(None, NonNull::from(&mut raw)) };
    check(status, "VTCopyVideoEncoderList", "listing video encoders")?;
    let Some(raw) = NonNull::new(raw.cast_mut()) else {
        return Ok(Vec::new());
    };
    // SAFETY: a Copy function returned it, so this takes over its +1 reference.
    // VTVideoEncoderList.h documents it as an array of CFDictionaries keyed by CFStrings.
    let list = unsafe {
        CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(CFRetained::from_raw(
            raw,
        ))
    };
    // SAFETY: VideoToolbox constants, set before any code runs and never written.
    let (codec_key, name_key, hardware_key) = unsafe {
        (
            kVTVideoEncoderList_CodecType,
            kVTVideoEncoderList_DisplayName,
            kVTVideoEncoderList_IsHardwareAccelerated,
        )
    };
    let mut encoders = Vec::new();
    for entry in list.iter() {
        let codec_type = entry
            .get(codec_key)
            .and_then(|v| v.downcast::<CFNumber>().ok())
            .and_then(|n| n.as_i64())
            .and_then(|n| u32::try_from(n).ok());
        let codec = match codec_type {
            Some(CM_H264) => Codec::H264,
            Some(CM_HEVC) => Codec::Hevc,
            _ => continue,
        };
        let name = entry
            .get(name_key)
            .and_then(|v| v.downcast::<CFString>().ok())
            .map_or_else(
                || format!("VideoToolbox {}", codec_name(codec)),
                |s| s.to_string(),
            );
        let hardware = entry
            .get(hardware_key)
            .and_then(|v| v.downcast::<CFBoolean>().ok())
            .is_some_and(|b| b.as_bool());
        encoders.push(CodecInfo {
            name,
            codec,
            role: CodecRole::VideoEncoder,
            hardware,
        });
    }
    Ok(encoders)
}

/// These run on any Mac, the CI runner included: VideoToolbox needs no permission, and
/// `HardwarePreference::Prefer` falls back to Apple's software encoder where a virtual
/// machine has no hardware one. Tests that need the hardware encoder are `hw-tests`.
#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        Codec, ColorInfo, FrameSize, MediaTime, Packet, PixelFormat, Timebase, VideoFrame,
        VideoFrameInfo,
    };
    use jstream_platform::{
        Codecs, HardwarePreference, PlatformError, RateControl, VideoEncoder, VideoEncoderConfig,
        VideoSurface,
    };

    use crate::{VideoToolboxCodecs, timebase};

    fn config(codec: Codec, format: PixelFormat) -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec,
            size: FrameSize::new(320, 240).expect("valid size"),
            input_format: format,
            frame_period: Timebase::new(1, 30).expect("valid period"),
            rate_control: RateControl::Cbr { kbps: 1000 },
            keyframe_interval: 30,
            hardware: HardwarePreference::Prefer,
        }
    }

    /// Frame `n` of a moving gradient, at `n` thirtieths of a second on the media clock.
    fn frame(config: &VideoEncoderConfig, n: u32) -> VideoSurface {
        let pts = MediaTime::new(i64::from(n) * 333_333, timebase());
        let mut frame = VideoFrame::zeroed(VideoFrameInfo {
            size: config.size,
            format: config.input_format,
            color: ColorInfo::default(),
            pts,
        })
        .expect("frame");
        for y in 0..config.size.height() {
            if let Some(row) = frame.row_mut(0, y) {
                for (x, px) in row.iter_mut().enumerate() {
                    *px = u8::try_from((x + y as usize + n as usize * 8) % 256).unwrap_or(0);
                }
            }
        }
        VideoSurface::Cpu(frame)
    }

    /// Encodes `frames` frames, asking for a keyframe before frame `keyframe_at`.
    fn encode(
        encoder: &mut dyn VideoEncoder,
        config: &VideoEncoderConfig,
        frames: u32,
        keyframe_at: Option<u32>,
    ) -> Vec<Packet> {
        let mut out = Vec::new();
        for n in 0..frames {
            // A full queue must drain by itself: fail, rather than hang, if it doesn't.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !encoder.can_accept() {
                encoder.poll(&mut out).expect("poll");
                assert!(
                    std::time::Instant::now() < deadline,
                    "the encoder still holds every queued frame before frame {n}"
                );
                std::thread::yield_now();
            }
            if keyframe_at == Some(n) {
                encoder.request_keyframe().expect("keyframe request");
            }
            encoder.encode(frame(config, n)).expect("encode");
            encoder.poll(&mut out).expect("poll");
        }
        encoder.finish(&mut out).expect("finish");
        out
    }

    /// The NAL unit types of an Annex B packet (H.264: the low five bits; HEVC: bits 1–6).
    fn nal_types(packet: &Packet, codec: Codec) -> Vec<u8> {
        let data = packet.data();
        assert_eq!(&data[..4], &[0, 0, 0, 1], "starts with a start code");
        data.windows(5)
            .filter(|w| w[..4] == [0, 0, 0, 1])
            .map(|w| match codec {
                Codec::Hevc => (w[4] >> 1) & 0x3F,
                _ => w[4] & 0x1F,
            })
            .collect()
    }

    fn check_stream(packets: &[Packet], codec: Codec, frames: u32) {
        assert_eq!(packets.len(), frames as usize, "one packet per frame");
        let first = &packets[0];
        assert!(first.is_keyframe(), "the stream opens on a keyframe");
        let types = nal_types(first, codec);
        let wanted: &[u8] = match codec {
            Codec::Hevc => &[32, 33, 34],
            _ => &[7, 8],
        };
        for t in wanted {
            assert!(
                types.contains(t),
                "keyframe carries parameter set {t}: {types:?}"
            );
        }
        for (n, p) in packets.iter().enumerate() {
            assert_eq!(p.codec(), codec);
            let want = MediaTime::new(i64::try_from(n).expect("small") * 333_333, timebase());
            assert_eq!(p.pts(), want, "packet {n} keeps its frame's time");
            assert_eq!(p.dts(), p.pts(), "no reordering");
        }
    }

    #[test]
    fn h264_encoders_are_listed() {
        let list = VideoToolboxCodecs::new().list().expect("list");
        assert!(list.iter().any(|c| c.codec == Codec::H264), "{list:?}");
    }

    #[test]
    fn nv12_frames_encode_to_annex_b_h264() {
        let config = config(Codec::H264, PixelFormat::Nv12);
        let mut encoder = VideoToolboxCodecs::new()
            .video_encoder(&config)
            .expect("an H.264 session");
        assert_eq!(encoder.info().codec, Codec::H264);
        let packets = encode(encoder.as_mut(), &config, 10, None);
        check_stream(&packets, Codec::H264, 10);
    }

    #[test]
    fn bgra_frames_and_software_encoding_work() {
        let mut config = config(Codec::H264, PixelFormat::Bgra8);
        config.hardware = HardwarePreference::Avoid;
        config.rate_control = RateControl::Vbr {
            kbps: 800,
            max_kbps: 1600,
        };
        let mut encoder = VideoToolboxCodecs::new()
            .video_encoder(&config)
            .expect("a software H.264 session");
        assert!(!encoder.info().hardware, "{:?}", encoder.info());
        let packets = encode(encoder.as_mut(), &config, 5, None);
        check_stream(&packets, Codec::H264, 5);
    }

    #[test]
    fn hevc_encodes_where_an_encoder_is_listed() {
        let codecs = VideoToolboxCodecs::new();
        let list = codecs.list().expect("list");
        if !list.iter().any(|c| c.codec == Codec::Hevc) {
            return;
        }
        let config = config(Codec::Hevc, PixelFormat::Nv12);
        let mut encoder = codecs.video_encoder(&config).expect("an HEVC session");
        let packets = encode(encoder.as_mut(), &config, 5, None);
        check_stream(&packets, Codec::Hevc, 5);
    }

    #[test]
    fn a_requested_keyframe_comes_next() {
        let mut config = config(Codec::H264, PixelFormat::Nv12);
        config.keyframe_interval = 1000;
        let mut encoder = VideoToolboxCodecs::new()
            .video_encoder(&config)
            .expect("an H.264 session");
        let packets = encode(encoder.as_mut(), &config, 8, Some(5));
        let keyframes: Vec<usize> = packets
            .iter()
            .enumerate()
            .filter_map(|(n, p)| p.is_keyframe().then_some(n))
            .collect();
        assert_eq!(keyframes, [0, 5]);
    }

    #[test]
    fn mismatched_frames_and_use_after_finish_are_refused() {
        let config = config(Codec::H264, PixelFormat::Nv12);
        let mut encoder = VideoToolboxCodecs::new()
            .video_encoder(&config)
            .expect("an H.264 session");
        let other = VideoEncoderConfig {
            size: FrameSize::new(640, 480).expect("valid size"),
            ..config
        };
        assert!(matches!(
            encoder.encode(frame(&other, 0)),
            Err(PlatformError::Unsupported { .. })
        ));
        let bgra = VideoEncoderConfig {
            input_format: PixelFormat::Bgra8,
            ..config
        };
        assert!(matches!(
            encoder.encode(frame(&bgra, 0)),
            Err(PlatformError::Unsupported { .. })
        ));
        let mut out = Vec::new();
        encoder.finish(&mut out).expect("finish");
        assert!(out.is_empty());
        assert!(!encoder.can_accept());
        assert!(matches!(
            encoder.encode(frame(&config, 0)),
            Err(PlatformError::InvalidState { .. })
        ));
        assert!(matches!(
            encoder.request_keyframe(),
            Err(PlatformError::InvalidState { .. })
        ));
    }

    #[test]
    fn configurations_videotoolbox_cant_take_are_refused_before_opening() {
        let codecs = VideoToolboxCodecs::new();
        for config in [
            config(Codec::Av1, PixelFormat::Nv12),
            config(Codec::H264, PixelFormat::P010),
        ] {
            assert!(
                matches!(
                    codecs.video_encoder(&config),
                    Err(PlatformError::Unsupported { .. })
                ),
                "{config:?}"
            );
        }
    }

    /// Needs a Mac with the hardware encoder (any Apple Silicon Mac, not a VM).
    #[cfg(feature = "hw-tests")]
    #[test]
    #[ignore = "needs the hardware encoder; run with --features hw-tests -- --ignored"]
    fn the_hardware_encoder_opens_when_required() {
        for codec in [Codec::H264, Codec::Hevc] {
            let mut config = config(codec, PixelFormat::Nv12);
            config.hardware = HardwarePreference::Require;
            let mut encoder = VideoToolboxCodecs::new()
                .video_encoder(&config)
                .expect("a hardware session");
            assert!(encoder.info().hardware, "{:?}", encoder.info());
            let packets = encode(encoder.as_mut(), &config, 10, None);
            check_stream(&packets, codec, 10);
        }
    }
}
