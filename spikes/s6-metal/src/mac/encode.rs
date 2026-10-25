//! The consumer's encoder: a VideoToolbox H.264 session fed the engine's IOSurface-backed
//! NV12 pixel buffers directly, as the engine's encoder will be once encode targets are
//! IOSurfaces. Each frame's output callback gives its ring slot back, because VideoToolbox
//! reads the surface until then.

use std::ffi::{c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use objc2_core_foundation::{CFBoolean, CFNumber, CFRetained, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMTimeInvalid,
};
use objc2_core_video::CVPixelBuffer;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionCopyProperty, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxFrameDelayCount,
    kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
};

use crate::err::{Context, Result, bail};

/// `kCMVideoCodecType_H264`.
const H264: u32 = u32::from_be_bytes(*b"avc1");
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// What the output callback reaches through its `refcon`.
struct Shared {
    /// The Annex B stream so far.
    out: Mutex<Vec<u8>>,
    /// Ring slots VideoToolbox has finished with.
    freed: Sender<usize>,
    encoded: AtomicU64,
    failed: AtomicU64,
}

pub struct Encoder {
    /// Declared first, so it is invalidated before `shared` is freed.
    session: CFRetained<VTCompressionSession>,
    shared: Box<Shared>,
    fps: i32,
    pub hardware: bool,
}

// SAFETY: a compression session has no thread affinity, and `Shared` is only reached
// through atomics, a mutex and a channel.
unsafe impl Send for Encoder {}
// SAFETY: as above. The consumer thread is the only one that encodes; the tick thread
// calls `finish` only after the consumer has been joined.
unsafe impl Sync for Encoder {}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the session is live; after this no callback runs, so `shared` can go.
        unsafe { self.session.invalidate() };
    }
}

impl Encoder {
    pub fn new(
        (width, height): (u32, u32),
        fps: u32,
        bitrate_kbps: u32,
        freed: Sender<usize>,
    ) -> Result<Self> {
        let shared = Box::new(Shared {
            out: Mutex::new(Vec::new()),
            freed,
            encoded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        });
        let mut raw: *mut VTCompressionSession = ptr::null_mut();
        // SAFETY: no encoder specification or source attributes (the frames come in as
        // they are); `output` has the VTCompressionOutputCallback signature and reads
        // `refcon` only as `&Shared`, the boxed value that outlives the session; `raw` is
        // a valid out pointer.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                H264,
                None,
                None,
                None,
                Some(output),
                ptr::from_ref::<Shared>(&shared).cast_mut().cast(),
                NonNull::from(&mut raw),
            )
        };
        if status != 0 {
            bail!("VTCompressionSessionCreate failed: {status}");
        }
        let raw = NonNull::new(raw).context("VTCompressionSessionCreate returned no session")?;
        // SAFETY: a Create function returned it, so this takes over its +1 reference.
        let session = unsafe { CFRetained::from_raw(raw) };
        let set = |key, value: &CFType, what: &str| -> Result<()> {
            // SAFETY: the session, the constant key and the value are live, and each value
            // has the type VTCompressionProperties.h documents for its key.
            let status = unsafe { VTSessionSetProperty(&session, key, Some(value)) };
            if status != 0 {
                bail!("setting {what}: {status}");
            }
            Ok(())
        };
        // SAFETY: VideoToolbox constants, set before any code runs and never written.
        let (real_time, reordering, delay, rate, average) = unsafe {
            (
                kVTCompressionPropertyKey_RealTime,
                kVTCompressionPropertyKey_AllowFrameReordering,
                kVTCompressionPropertyKey_MaxFrameDelayCount,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                kVTCompressionPropertyKey_AverageBitRate,
            )
        };
        set(real_time, CFBoolean::new(true), "real-time encoding")?;
        set(reordering, CFBoolean::new(false), "no frame reordering")?;
        // As the engine's encoder does (#633); an encoder without the property keeps its own.
        let _ = set(delay, &CFNumber::new_i32(3), "the maximum frame delay");
        set(rate, &CFNumber::new_i32(fps as i32), "the frame rate")?;
        let bps = i64::from(bitrate_kbps) * 1000;
        set(average, &CFNumber::new_i64(bps), "the average bit rate")?;
        // SAFETY: the session is live and configured.
        let status = unsafe { session.prepare_to_encode_frames() };
        if status != 0 {
            bail!("VTCompressionSessionPrepareToEncodeFrames failed: {status}");
        }
        let hardware = uses_hardware(&session);
        Ok(Self {
            session,
            shared,
            fps: fps as i32,
            hardware,
        })
    }

    /// Submits frame `n` from ring slot `slot`. The slot comes back on the `freed` channel
    /// when VideoToolbox is done with it.
    pub fn encode(&self, buffer: &CVPixelBuffer, n: u64, slot: usize) -> Result<()> {
        let pts = CMTime {
            value: n as i64,
            timescale: self.fps,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        let duration = CMTime {
            value: 1,
            timescale: self.fps,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        // SAFETY: the session and the pixel buffer are live; no frame options; the frame
        // refcon is the slot number plus one, never dereferenced; the info-flags pointer
        // may be null.
        let status = unsafe {
            self.session.encode_frame(
                buffer,
                pts,
                duration,
                None,
                (slot + 1) as *mut c_void,
                ptr::null_mut(),
            )
        };
        if status != 0 {
            bail!("VTCompressionSessionEncodeFrame failed: {status}");
        }
        Ok(())
    }

    /// Gives `slot` back to the ring, for a frame VideoToolbox refused and won't call back
    /// for.
    pub fn release(&self, slot: usize) {
        let _ = self.shared.freed.send(slot);
    }

    /// Completes every pending frame and returns the stream, the frames encoded and the
    /// frames that failed.
    pub fn finish(&self) -> Result<(Vec<u8>, u64, u64)> {
        // SAFETY: the session is live; an invalid time completes every pending frame.
        let status = unsafe { self.session.complete_frames(kCMTimeInvalid) };
        if status != 0 {
            bail!("VTCompressionSessionCompleteFrames failed: {status}");
        }
        let out =
            std::mem::take(
                &mut *self.shared.out.lock().map_err(|_| {
                    crate::err::Error::msg("the encoder's output lock was poisoned")
                })?,
            );
        Ok((
            out,
            self.shared.encoded.load(Ordering::Acquire),
            self.shared.failed.load(Ordering::Acquire),
        ))
    }
}

/// Whether the session uses a hardware encoder; `false` when it can't be read.
fn uses_hardware(session: &VTCompressionSession) -> bool {
    let mut value: *const CFType = ptr::null();
    // SAFETY: the session and the constant key are live, and `value` is a valid out
    // pointer for the +1 CFTypeRef written on success.
    let status = unsafe {
        VTSessionCopyProperty(
            session,
            kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
            None,
            ptr::from_mut(&mut value).cast(),
        )
    };
    if status != 0 {
        return false;
    }
    NonNull::new(value.cast_mut())
        // SAFETY: a Copy function returned it, so this takes over its +1 reference.
        .map(|p| unsafe { CFRetained::from_raw(p) })
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .is_some_and(|b| b.as_bool())
}

/// VideoToolbox's output callback, on its own thread. No panic leaves it.
unsafe extern "C-unwind" fn output(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `refcon` is the boxed `Shared` the session was made with, which outlives
        // the session.
        let Some(shared) = (unsafe { refcon.cast::<Shared>().as_ref() }) else {
            return;
        };
        // SAFETY: VideoToolbox passes a sample valid for this call, or null.
        let sample = unsafe { sample.as_ref() };
        let ok = status == 0
            && !flags.contains(VTEncodeInfoFlags::FrameDropped)
            && sample.is_some_and(|s| append(shared, s).is_ok());
        if ok {
            shared.encoded.fetch_add(1, Ordering::AcqRel);
        } else {
            shared.failed.fetch_add(1, Ordering::AcqRel);
        }
        let slot = (frame_refcon as usize).wrapping_sub(1);
        let _ = shared.freed.send(slot);
    }));
}

/// Appends one encoded sample to the stream as Annex B, with the parameter sets in front
/// of every keyframe.
fn append(shared: &Shared, sample: &CMSampleBuffer) -> Result<()> {
    // SAFETY: the sample is live for the callback; these getters only read it, and the
    // block buffer and format description come back retained.
    let (block, desc) = unsafe { (sample.data_buffer(), sample.format_description()) };
    let block = block.context("an encoded frame without data")?;
    let desc = desc.context("an encoded frame without a format description")?;
    // SAFETY: the block buffer is live.
    let len = unsafe { block.data_length() };
    let mut data = vec![0u8; len];
    if len > 0 {
        // SAFETY: `data` is `len` writable bytes, the block buffer's whole length.
        let status =
            unsafe { block.copy_data_bytes(0, len, NonNull::from(data.as_mut_slice()).cast()) };
        if status != 0 {
            bail!("CMBlockBufferCopyDataBytes failed: {status}");
        }
    }
    let (sets, nal_length) = parameter_sets(&desc)?;
    let mut au = Vec::with_capacity(len + 64);
    // Every frame carries the parameter sets: simpler than reading the sync attachment,
    // and harmless in a raw stream a player reads.
    for set in &sets {
        au.extend_from_slice(&START_CODE);
        au.extend_from_slice(set);
    }
    let mut rest = data.as_slice();
    while !rest.is_empty() {
        let (prefix, tail) = rest
            .split_at_checked(nal_length)
            .context("a truncated NAL unit length")?;
        let n = prefix
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        let (unit, tail) = tail.split_at_checked(n).context("a truncated NAL unit")?;
        au.extend_from_slice(&START_CODE);
        au.extend_from_slice(unit);
        rest = tail;
    }
    shared
        .out
        .lock()
        .map_err(|_| crate::err::Error::msg("the encoder's output lock was poisoned"))?
        .extend_from_slice(&au);
    Ok(())
}

/// The format description's SPS and PPS, and its NAL unit length size.
fn parameter_sets(desc: &CMFormatDescription) -> Result<(Vec<Vec<u8>>, usize)> {
    let mut count = 0usize;
    let mut nal_length: c_int = 0;
    // SAFETY: `desc` is live; null set and size pointers are allowed, and `count` and
    // `nal_length` are valid out pointers.
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            desc,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut count,
            &mut nal_length,
        )
    };
    if status != 0 {
        bail!("reading the parameter sets failed: {status}");
    }
    let mut sets = Vec::with_capacity(count);
    for index in 0..count {
        let mut set: *const u8 = ptr::null();
        let mut size = 0usize;
        // SAFETY: as above, with valid out pointers for the set and its size.
        let status = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                desc,
                index,
                &mut set,
                &mut size,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != 0 || set.is_null() || size == 0 {
            continue;
        }
        // SAFETY: Core Media returns `size` bytes inside the live description.
        sets.push(unsafe { slice::from_raw_parts(set, size) }.to_vec());
    }
    let nal_length = usize::try_from(nal_length).unwrap_or(4);
    if !matches!(nal_length, 1 | 2 | 4) {
        bail!("NAL unit length size {nal_length}");
    }
    Ok((sets, nal_length))
}
