//! The VideoToolbox H.264 and HEVC decoder (ADR-0063 §6): a `VTDecompressionSession` fed
//! the engine's Annex B packets, whose frames come back as CPU frames in presentation
//! order.
//!
//! The decoder is opened with [`vt_decoder_settings`] (hardware by default, Apple's
//! software decoder for `HardwarePreference::Avoid`), but its session waits for the first
//! packet with a complete set of parameter sets: a format description is made from them,
//! and a new session whenever a keyframe brings different ones (a resolution change).
//! Packets before that are dropped, as a decoder joining mid-stream drops them.
//!
//! Each packet is split by [`split_access_unit`] into its parameter sets and a
//! length-prefixed sample, wrapped in a `CMSampleBuffer` with its times, and decoded
//! synchronously: no asynchronous or temporal-processing flags, and the session is waited
//! on before [`decode`](VideoDecoder::decode) returns, so every frame of the packet has
//! reached [`output_callback`] by then. The callback copies the frame out of its pixel
//! buffer into a CPU frame in the configured format and sends it down a channel; frames
//! come in decode order, and [`Reorder`] holds them until they can leave in presentation
//! order. [`poll`](VideoDecoder::poll) never blocks.
//!
//! GPU frames (the IOSurface-backed pixel buffers imported into Metal) follow once the
//! Metal interop spike lands (ADR-0063 §8).

use std::ffi::{c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};

use jstream_platform::media::{
    Codec, ColorInfo, FrameSize, MediaTime, Packet, Plane, Rounding, Timebase, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{
    CodecInfo, CodecRole, PlatformError, VideoDecoder, VideoDecoderConfig, VideoSurface,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
    kCMTimeInvalid,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferIsPlanar, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTSessionCopyProperty,
    kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
    kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};

use crate::annexb::{cm_time_parts, copy_rows, media_time_from_cm};
use crate::codec::{VtDecoderSettings, vt_decoder_settings};
use crate::decode::{Reorder, sets_complete, split_access_unit};
use crate::fourcc::{os_status, pixel_format_from_cv};

/// Decoded frames waiting for [`poll`](VideoDecoder::poll), at most; beyond that
/// [`can_accept`](VideoDecoder::can_accept) says no. Frames held for reordering don't
/// count.
const MAX_PENDING: usize = 8;

/// `noErr`.
const NO_ERR: i32 = 0;

/// The NAL unit length size the samples are written with ([`split_access_unit`]).
const NAL_LENGTH_SIZE: c_int = 4;

fn check(status: i32, call: &'static str, what: &str) -> Result<(), PlatformError> {
    if status == NO_ERR {
        Ok(())
    } else {
        Err(os_status(call, status, what))
    }
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::Hevc => "HEVC",
        _ => "H.264",
    }
}

/// What the output callback hands to the decoder.
#[derive(Debug)]
enum Decoded {
    /// One frame, copied out, with its presentation time.
    Frame(VideoFrame),
    /// VideoToolbox dropped the frame (a leading frame after a seek, say).
    Dropped,
    /// Decoding the frame failed.
    Failed(PlatformError),
}

/// What the output callback reaches through its `refcon`. Owned by [`VtDecoder`], boxed
/// so its address is fixed, and dropped only after every session is torn down.
struct CallbackContext {
    sender: Sender<Decoded>,
    /// The time base frames are timed in: the first packet's.
    timebase: OnceLock<Timebase>,
}

/// A `VTDecompressionSession` that is waited on and invalidated when dropped, so no
/// callback runs after it.
struct Session(CFRetained<VTDecompressionSession>);

// SAFETY: a decompression session has no thread affinity: it needs no run loop and is not
// tied to the thread that created it, and VideoToolbox runs its callbacks on its own
// threads. `Session` is not `Sync`, so only the thread that owns the decoder calls it.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session is live. Waiting for every pending frame before
        // invalidating means no output callback is pending when the context it uses is
        // freed after us.
        unsafe {
            let _ = self.0.wait_for_asynchronous_frames();
            self.0.invalidate();
        }
    }
}

/// A pixel buffer locked for reading, unlocked when dropped.
struct ReadLocked<'a>(&'a CVPixelBuffer);

impl<'a> ReadLocked<'a> {
    fn lock(buffer: &'a CVPixelBuffer) -> Result<Self, PlatformError> {
        // SAFETY: the buffer is live; a read-only lock lets us read its planes.
        let status =
            unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
        check(
            status,
            "CVPixelBufferLockBaseAddress",
            "locking a decoded frame",
        )?;
        Ok(Self(buffer))
    }

    /// Plane `index`'s bytes (the whole buffer for a packed format) and its stride.
    fn plane(&self, index: usize) -> Option<(&[u8], usize)> {
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
        // points at `rows` rows of `stride` bytes it owns. The borrow is tied to `&self`,
        // so it ends before the unlock.
        Some((unsafe { slice::from_raw_parts(base.as_ptr(), len) }, stride))
    }
}

impl Drop for ReadLocked<'_> {
    fn drop(&mut self) {
        // SAFETY: this buffer was locked with the same (read-only) flags in `lock`.
        let _ = unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

/// `buffer`'s picture as a tightly packed CPU frame presented at `pts`.
pub(crate) fn copy_image(
    buffer: &CVPixelBuffer,
    pts: MediaTime,
) -> Result<VideoFrame, PlatformError> {
    let code = CVPixelBufferGetPixelFormatType(buffer);
    let format = pixel_format_from_cv(code).ok_or_else(|| PlatformError::Unsupported {
        what: format!("a decoded frame in Core Video format {code:#010x}"),
    })?;
    let too_big = || PlatformError::Unsupported {
        what: "a decoded frame wider or taller than 4 G pixels".into(),
    };
    let width = u32::try_from(CVPixelBufferGetWidth(buffer)).map_err(|_| too_big())?;
    let height = u32::try_from(CVPixelBufferGetHeight(buffer)).map_err(|_| too_big())?;
    let info = VideoFrameInfo {
        size: FrameSize::new(width, height)?,
        format,
        color: ColorInfo::default(),
        pts,
    };
    let locked = ReadLocked::lock(buffer)?;
    let mut planes = Vec::with_capacity(format.plane_count());
    for index in 0..format.plane_count() {
        let Some(geometry) = info.plane(index)? else {
            break;
        };
        let (src, src_stride) = locked
            .plane(index)
            .ok_or_else(|| PlatformError::Unsupported {
                what: format!("plane {index} of a decoded {format:?} frame"),
            })?;
        let rows = usize::try_from(geometry.height).unwrap_or(usize::MAX);
        let mut data = vec![0u8; geometry.row_bytes * rows];
        copy_rows(
            src,
            src_stride,
            &mut data,
            geometry.row_bytes,
            geometry.row_bytes,
            rows,
        )?;
        planes.push(Plane::new(data, geometry.row_bytes));
    }
    Ok(VideoFrame::new(info, planes)?)
}

/// What the callback reports for one frame.
fn decoded(
    context: &CallbackContext,
    status: i32,
    flags: VTDecodeInfoFlags,
    image: Option<&CVImageBuffer>,
    pts: CMTime,
) -> Decoded {
    if status != NO_ERR {
        return Decoded::Failed(os_status(
            "VTDecompressionOutputCallback",
            status,
            "decoding a frame",
        ));
    }
    let Some(image) = image.filter(|_| !flags.contains(VTDecodeInfoFlags::FrameDropped)) else {
        return Decoded::Dropped;
    };
    let Some(&timebase) = context.timebase.get() else {
        return Decoded::Failed(PlatformError::InvalidState {
            what: "VideoToolbox output",
            state: "no packet was decoded",
        });
    };
    if !pts.flags.contains(CMTimeFlags::Valid) {
        return Decoded::Failed(PlatformError::Os {
            call: "VTDecompressionOutputCallback",
            code: None,
            message: "a decoded frame without a presentation time".into(),
        });
    }
    media_time_from_cm(pts.value, pts.timescale, timebase)
        .and_then(|pts| copy_image(image, pts))
        .map_or_else(Decoded::Failed, Decoded::Frame)
}

/// VideoToolbox's output callback, once per decoded frame, in decode order. No panic
/// leaves it (R-UNSAFE-005).
unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _source_frame_refcon: *mut c_void,
    status: i32,
    flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    pts: CMTime,
    _duration: CMTime,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `refcon` is the boxed `CallbackContext` the session was created with,
        // which outlives every session (`VtDecoder` drops its session first).
        let Some(context) = (unsafe { refcon.cast::<CallbackContext>().as_ref() }) else {
            return;
        };
        // SAFETY: VideoToolbox passes an image buffer valid for this call, or null.
        let image = unsafe { image.as_ref() };
        let out = catch_unwind(AssertUnwindSafe(|| {
            decoded(context, status, flags, image, pts)
        }))
        .unwrap_or_else(|_| {
            Decoded::Failed(PlatformError::Os {
                call: "VTDecompressionOutputCallback",
                code: None,
                message: "copying a decoded frame panicked".into(),
            })
        });
        // A closed channel means the decoder is being dropped; the frame goes with it.
        let _ = context.sender.send(out);
    }));
}

/// A format description for `codec` from its parameter sets (VPS, SPS, PPS), for samples
/// with 4-byte NAL unit lengths.
fn format_description(
    codec: Codec,
    sets: &[&[u8]],
) -> Result<CFRetained<CMFormatDescription>, PlatformError> {
    let pointers: Vec<NonNull<u8>> = sets.iter().map(|s| NonNull::from(*s).cast()).collect();
    let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
    let (Some(pointers_ptr), Some(sizes_ptr)) = (
        NonNull::new(pointers.as_ptr().cast_mut()),
        NonNull::new(sizes.as_ptr().cast_mut()),
    ) else {
        return Err(PlatformError::Os {
            call: "CMVideoFormatDescriptionCreateFromParameterSets",
            code: None,
            message: "no parameter sets".into(),
        });
    };
    let mut raw: *const CMFormatDescription = ptr::null();
    // SAFETY: `pointers` and `sizes` hold `sets.len()` entries describing byte slices
    // that outlive the call (Core Media copies them); `raw` is a valid out pointer for
    // the +1 retained description.
    let status = unsafe {
        match codec {
            Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                None,
                sets.len(),
                pointers_ptr,
                sizes_ptr,
                NAL_LENGTH_SIZE,
                None,
                NonNull::from(&mut raw),
            ),
            _ => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                sets.len(),
                pointers_ptr,
                sizes_ptr,
                NAL_LENGTH_SIZE,
                NonNull::from(&mut raw),
            ),
        }
    };
    check(
        status,
        "CMVideoFormatDescriptionCreateFromParameterSets",
        &format!("reading the stream's {} parameter sets", codec_name(codec)),
    )?;
    let raw = NonNull::new(raw.cast_mut()).ok_or_else(|| PlatformError::Os {
        call: "CMVideoFormatDescriptionCreateFromParameterSets",
        code: None,
        message: "returned no format description".into(),
    })?;
    // SAFETY: a Create function returned it, so this takes over its +1 reference.
    Ok(unsafe { CFRetained::from_raw(raw) })
}

fn cm_time(time: MediaTime) -> Result<CMTime, PlatformError> {
    let (value, timescale) = cm_time_parts(time).ok_or_else(|| PlatformError::Unsupported {
        what: format!("a packet time in {} on VideoToolbox", time.timebase()),
    })?;
    Ok(CMTime {
        value,
        timescale,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    })
}

/// One sample of `data` (length-prefixed NAL units) described by `desc`, with its times.
fn sample_buffer(
    desc: &CMFormatDescription,
    data: &[u8],
    pts: CMTime,
    dts: CMTime,
) -> Result<CFRetained<CMSampleBuffer>, PlatformError> {
    let len = data.len();
    let mut block: *mut CMBlockBuffer = ptr::null_mut();
    // SAFETY: no memory block is passed, so Core Media allocates `len` bytes now (the
    // flag) with the default allocator; `block` is a valid out pointer for the +1
    // retained buffer.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            ptr::null_mut(),
            len,
            None,
            ptr::null(),
            0,
            len,
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block),
        )
    };
    check(
        status,
        "CMBlockBufferCreateWithMemoryBlock",
        "allocating a sample",
    )?;
    let block = NonNull::new(block).ok_or_else(|| PlatformError::Os {
        call: "CMBlockBufferCreateWithMemoryBlock",
        code: None,
        message: "returned no block buffer".into(),
    })?;
    // SAFETY: a Create function returned it, so this takes over its +1 reference.
    let block = unsafe { CFRetained::from_raw(block) };
    // SAFETY: `data` is `len` readable bytes, and the block buffer holds `len` bytes from
    // offset 0.
    let status =
        unsafe { CMBlockBuffer::replace_data_bytes(NonNull::from(data).cast(), &block, 0, len) };
    check(status, "CMBlockBufferReplaceDataBytes", "filling a sample")?;
    let timing = CMSampleTimingInfo {
        // SAFETY: a Core Media constant, set before any code runs and never written.
        duration: unsafe { kCMTimeInvalid },
        presentationTimeStamp: pts,
        decodeTimeStamp: dts,
    };
    let mut sample: *mut CMSampleBuffer = ptr::null_mut();
    // SAFETY: the block buffer and format description are live; one sample, with one
    // timing entry and one size entry, each pointing at a value that outlives the call;
    // `sample` is a valid out pointer for the +1 retained sample.
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(desc),
            1,
            1,
            &timing,
            1,
            &len,
            NonNull::from(&mut sample),
        )
    };
    check(status, "CMSampleBufferCreateReady", "wrapping a sample")?;
    let sample = NonNull::new(sample).ok_or_else(|| PlatformError::Os {
        call: "CMSampleBufferCreateReady",
        code: None,
        message: "returned no sample buffer".into(),
    })?;
    // SAFETY: a Create function returned it, so this takes over its +1 reference.
    Ok(unsafe { CFRetained::from_raw(sample) })
}

/// `kVTVideoDecoderSpecification_*` for `settings`.
fn decoder_specification(
    settings: &VtDecoderSettings,
) -> CFRetained<CFDictionary<CFString, CFType>> {
    // SAFETY: VideoToolbox constants, set before any code runs and never written.
    let (enable, require) = unsafe {
        (
            kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
            kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
        )
    };
    let values: [&CFType; 2] = [
        CFBoolean::new(settings.enable_hardware),
        CFBoolean::new(settings.require_hardware),
    ];
    CFDictionary::from_slices(&[enable, require], &values)
}

/// The decoded frames' pixel buffers: the output format, IOSurface-backed.
fn destination_attributes(pixel_format: u32) -> CFRetained<CFDictionary<CFString, CFType>> {
    // SAFETY: Core Video constants, set before any code runs and never written.
    let keys = unsafe {
        [
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferIOSurfacePropertiesKey,
        ]
    };
    let format = CFNumber::new_i64(i64::from(pixel_format));
    let iosurface = CFDictionary::<CFString, CFType>::empty();
    let values: [&CFType; 2] = [&format, &iosurface];
    CFDictionary::from_slices(&keys, &values)
}

/// The session's `UsingHardwareAcceleratedVideoDecoder` property, `false` when it can't be
/// read.
fn uses_hardware(session: &VTDecompressionSession) -> bool {
    let mut value: *const CFType = ptr::null();
    // SAFETY: the session and the constant key are live, and `value` is a valid out
    // pointer for the CFTypeRef VideoToolbox writes, +1 retained, on success.
    let status = unsafe {
        VTSessionCopyProperty(
            session,
            kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
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

/// The open session and the format it decodes.
struct Open {
    /// Declared first, so it is torn down before the description.
    session: Session,
    desc: CFRetained<CMFormatDescription>,
    /// The parameter sets the description was made from.
    sets: Vec<Vec<u8>>,
}

/// One VideoToolbox H.264 or HEVC decoder, delivering CPU frames.
pub(crate) struct VtDecoder {
    /// Declared before `context`, so its session is waited on and invalidated before the
    /// context its callback uses is freed.
    open: Option<Open>,
    context: Box<CallbackContext>,
    receiver: Receiver<Decoded>,
    settings: VtDecoderSettings,
    config: VideoDecoderConfig,
    info: CodecInfo,
    reorder: Reorder<VideoSurface>,
    pending: Vec<VideoSurface>,
    finished: bool,
}

impl VtDecoder {
    /// A decoder for `config`. Its session opens on the first packet with parameter sets.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a configuration VideoToolbox can't take.
    pub(crate) fn open(config: &VideoDecoderConfig) -> Result<Self, PlatformError> {
        let settings = vt_decoder_settings(config)?;
        let (sender, receiver) = mpsc::channel();
        let kind = if settings.enable_hardware {
            "hardware"
        } else {
            "software"
        };
        Ok(Self {
            open: None,
            context: Box::new(CallbackContext {
                sender,
                timebase: OnceLock::new(),
            }),
            receiver,
            settings,
            config: *config,
            info: CodecInfo {
                name: format!("VideoToolbox {} decoder ({kind})", codec_name(config.codec)),
                codec: config.codec,
                role: CodecRole::VideoDecoder,
                hardware: settings.enable_hardware,
            },
            reorder: Reorder::default(),
            pending: Vec::new(),
            finished: false,
        })
    }

    /// Opens a session for `sets`, after the old one has delivered its frames.
    fn reopen(&mut self, sets: &[&[u8]]) -> Result<(), PlatformError> {
        // Dropping the old session waits for its frames, which `drain` then collects.
        self.open = None;
        self.drain()?;
        let codec = self.config.codec;
        let desc = format_description(codec, sets)?;
        let specification = decoder_specification(&self.settings);
        let attributes = destination_attributes(self.settings.pixel_format);
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: ptr::from_ref::<CallbackContext>(&self.context)
                .cast_mut()
                .cast(),
        };
        let mut raw: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY: the description is live; both dictionaries map CFString keys to the CF
        // types VideoToolbox and Core Video document for them. `output_callback` has the
        // VTDecompressionOutputCallback signature and reads its refcon only as
        // `&CallbackContext`, the boxed context, which outlives every session (sessions
        // are dropped first). VideoToolbox copies the record. `raw` is a valid out pointer.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &desc,
                Some(specification.as_ref()),
                Some(attributes.as_ref()),
                &record,
                NonNull::from(&mut raw),
            )
        };
        let name = codec_name(codec);
        if status != NO_ERR && self.settings.require_hardware {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a hardware {name} decoder for this stream on this Mac ({})",
                    os_status("VTDecompressionSessionCreate", status, "opening")
                ),
            });
        }
        check(
            status,
            "VTDecompressionSessionCreate",
            "opening the decoder",
        )?;
        let raw = NonNull::new(raw).ok_or_else(|| PlatformError::Os {
            call: "VTDecompressionSessionCreate",
            code: None,
            message: "returned no session".into(),
        })?;
        // SAFETY: a Create function returned it, so this takes over its +1 reference.
        let session = Session(unsafe { CFRetained::from_raw(raw) });
        let hardware = uses_hardware(&session.0);
        let kind = if hardware { "hardware" } else { "software" };
        self.info.name = format!("VideoToolbox {name} decoder ({kind})");
        self.info.hardware = hardware;
        self.open = Some(Open {
            session,
            desc,
            sets: sets.iter().map(|s| s.to_vec()).collect(),
        });
        Ok(())
    }

    /// Moves what the callback sent into the reorder buffer. The first failure is
    /// returned, after every frame before it is kept.
    fn drain(&mut self) -> Result<(), PlatformError> {
        let mut failure = None;
        while let Ok(decoded) = self.receiver.try_recv() {
            match decoded {
                Decoded::Frame(frame) => {
                    let pts = frame.info().pts;
                    self.reorder.hold(pts, VideoSurface::Cpu(frame));
                }
                Decoded::Dropped => {}
                Decoded::Failed(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn refuse_after_finish(&self) -> Result<(), PlatformError> {
        if self.finished {
            Err(PlatformError::InvalidState {
                what: "VideoToolbox decode",
                state: "the decoder is finished",
            })
        } else {
            Ok(())
        }
    }
}

impl VideoDecoder for VtDecoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        !self.finished && self.pending.len() < MAX_PENDING
    }

    fn decode(&mut self, packet: &Packet) -> Result<(), PlatformError> {
        self.refuse_after_finish()?;
        let codec = self.config.codec;
        if packet.codec() != codec {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a {:?} packet in a VideoToolbox {} decoder",
                    packet.codec(),
                    codec_name(codec)
                ),
            });
        }
        if !self.can_accept() {
            return Err(PlatformError::Busy {
                what: "VideoToolbox decoder",
            });
        }
        let tb = *self
            .context
            .timebase
            .get_or_init(|| packet.pts().timebase());
        let pts = packet.pts().rescale(tb, Rounding::Nearest)?;
        let dts = packet.dts().rescale(tb, Rounding::Nearest)?;
        let au = split_access_unit(codec, packet.data())?;
        let changed = !au.parameter_sets.is_empty()
            && self.open.as_ref().is_none_or(|o| {
                o.sets.len() != au.parameter_sets.len()
                    || o.sets.iter().zip(&au.parameter_sets).any(|(a, b)| a != b)
            });
        if changed && sets_complete(codec, &au.parameter_sets) {
            self.reopen(&au.parameter_sets)?;
        }
        let Some(open) = &self.open else {
            // No parameter sets yet: a stream joined mid-way, dropped until a keyframe.
            return Ok(());
        };
        if !au.sample.is_empty() {
            let sample = sample_buffer(&open.desc, &au.sample, cm_time(pts)?, cm_time(dts)?)?;
            let mut info = VTDecodeInfoFlags::empty();
            // SAFETY: the session and sample are live; no flags asks for synchronous
            // decoding in decode order, the frame refcon may be null, and `info` is a
            // valid out pointer.
            let status = unsafe {
                open.session.0.decode_frame(
                    &sample,
                    VTDecodeFrameFlags::empty(),
                    ptr::null_mut(),
                    &mut info,
                )
            };
            check(
                status,
                "VTDecompressionSessionDecodeFrame",
                "decoding a packet",
            )?;
            // SAFETY: the session is live. Waiting makes sure every frame of this packet
            // has reached the callback, should the decoder have run asynchronously.
            let status = unsafe { open.session.0.wait_for_asynchronous_frames() };
            check(
                status,
                "VTDecompressionSessionWaitForAsynchronousFrames",
                "decoding a packet",
            )?;
        }
        self.drain()?;
        // Every later packet has `dts >= packet.dts()`, so its frame's pts is too.
        self.reorder.release(Some(dts), &mut self.pending);
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
        out.append(&mut self.pending);
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
        if !self.finished {
            self.finished = true;
            if let Some(open) = &self.open {
                // SAFETY: the session is live; it emits any frames it still holds, then
                // the wait makes sure their callbacks have run.
                let status = unsafe {
                    let status = open.session.0.finish_delayed_frames();
                    let _ = open.session.0.wait_for_asynchronous_frames();
                    status
                };
                check(
                    status,
                    "VTDecompressionSessionFinishDelayedFrames",
                    "draining the decoder",
                )?;
            }
            self.drain()?;
            self.reorder.release(None, &mut self.pending);
        }
        self.poll(out)
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        Codec, ColorInfo, FrameSize, MediaTime, Packet, PixelFormat, Timebase, VideoFrame,
        VideoFrameInfo,
    };
    use jstream_platform::{
        Codecs, HardwarePreference, PlatformError, RateControl, VideoDecoder, VideoDecoderConfig,
        VideoEncoderConfig, VideoSurface,
    };

    use crate::{VideoToolboxCodecs, timebase};

    const FRAMES: u32 = 12;

    /// `FRAMES` packets of a moving gradient from the VideoToolbox encoder, with a
    /// keyframe every six frames. `None` where the Mac has no encoder for `codec`.
    fn packets(codec: Codec) -> Option<Vec<Packet>> {
        let config = VideoEncoderConfig {
            codec,
            size: FrameSize::new(320, 240).expect("valid size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1, 30).expect("valid period"),
            rate_control: RateControl::Cbr { kbps: 1000 },
            keyframe_interval: 6,
            hardware: HardwarePreference::Prefer,
        };
        let mut encoder = VideoToolboxCodecs::new().video_encoder(&config).ok()?;
        let mut out = Vec::new();
        for n in 0..FRAMES {
            let mut frame = VideoFrame::zeroed(VideoFrameInfo {
                size: config.size,
                format: config.input_format,
                color: ColorInfo::default(),
                pts: MediaTime::new(i64::from(n) * 333_333, timebase()),
            })
            .expect("frame");
            for y in 0..config.size.height() {
                if let Some(row) = frame.row_mut(0, y) {
                    for (x, px) in row.iter_mut().enumerate() {
                        *px = u8::try_from((x + y as usize + n as usize * 8) % 256).unwrap_or(0);
                    }
                }
            }
            while !encoder.can_accept() {
                encoder.poll(&mut out).expect("poll");
                std::thread::yield_now();
            }
            encoder.encode(VideoSurface::Cpu(frame)).expect("encode");
            encoder.poll(&mut out).expect("poll");
        }
        encoder.finish(&mut out).expect("finish");
        Some(out)
    }

    fn decoder(codec: Codec, format: PixelFormat) -> Box<dyn VideoDecoder> {
        VideoToolboxCodecs::new()
            .video_decoder(&VideoDecoderConfig {
                codec,
                output_format: format,
                hardware: HardwarePreference::Prefer,
            })
            .expect("a decoder")
    }

    fn decode_all(decoder: &mut dyn VideoDecoder, packets: &[Packet]) -> Vec<VideoFrame> {
        let mut out = Vec::new();
        for p in packets {
            decoder.decode(p).expect("decode");
            decoder.poll(&mut out).expect("poll");
        }
        decoder.finish(&mut out).expect("finish");
        out.into_iter()
            .map(|s| match s {
                VideoSurface::Cpu(f) => f,
                other => panic!("a CPU frame, not {other:?}"),
            })
            .collect()
    }

    fn check_frames(frames: &[VideoFrame], packets: &[Packet], format: PixelFormat) {
        assert_eq!(frames.len(), packets.len(), "one frame per packet");
        for (f, p) in frames.iter().zip(packets) {
            let info = f.info();
            assert_eq!(info.format, format);
            assert_eq!((info.size.width(), info.size.height()), (320, 240));
            assert_eq!(info.pts, p.pts(), "in presentation order, with its time");
        }
    }

    #[test]
    fn h264_decodes_to_nv12_and_bgra() {
        let packets = packets(Codec::H264).expect("an H.264 encoder");
        for format in [PixelFormat::Nv12, PixelFormat::Bgra8] {
            let mut d = decoder(Codec::H264, format);
            assert_eq!(d.info().codec, Codec::H264);
            let frames = decode_all(d.as_mut(), &packets);
            check_frames(&frames, &packets, format);
        }
    }

    #[test]
    fn hevc_decodes_where_it_encodes() {
        let Some(packets) = packets(Codec::Hevc) else {
            return;
        };
        let mut d = decoder(Codec::Hevc, PixelFormat::Nv12);
        let frames = decode_all(d.as_mut(), &packets);
        check_frames(&frames, &packets, PixelFormat::Nv12);
    }

    #[test]
    fn a_decoder_joining_mid_stream_starts_at_the_next_keyframe() {
        let packets = packets(Codec::H264).expect("an H.264 encoder");
        let at = packets
            .iter()
            .skip(1)
            .position(Packet::is_keyframe)
            .expect("a second keyframe")
            + 1;
        let mut d = decoder(Codec::H264, PixelFormat::Nv12);
        let frames = decode_all(d.as_mut(), &packets[1..]);
        check_frames(&frames, &packets[at..], PixelFormat::Nv12);
    }

    #[test]
    fn mismatched_packets_and_use_after_finish_are_refused() {
        let packets = packets(Codec::H264).expect("an H.264 encoder");
        let mut d = decoder(Codec::Hevc, PixelFormat::Nv12);
        assert!(matches!(
            d.decode(&packets[0]),
            Err(PlatformError::Unsupported { .. })
        ));
        let mut d = decoder(Codec::H264, PixelFormat::Nv12);
        let mut out = Vec::new();
        d.finish(&mut out).expect("finish");
        assert!(!d.can_accept());
        assert!(matches!(
            d.decode(&packets[0]),
            Err(PlatformError::InvalidState { .. })
        ));
        let no_units = Packet::new(
            Codec::H264,
            false,
            packets[0].pts(),
            packets[0].dts(),
            vec![0u8, 0],
        )
        .expect("packet");
        let mut d = decoder(Codec::H264, PixelFormat::Nv12);
        assert!(d.decode(&no_units).is_err(), "no NAL units");
    }
}
