//! Reading a captured frame's pixel buffer on the CPU, for both kinds of capture:
//! ScreenCaptureKit's display and window frames ([`crate::capture`]) and AVFoundation's
//! camera frames ([`crate::video_device`]). One lock and one set of `SAFETY` arguments,
//! so the two can't drift apart.

use std::ptr::NonNull;
use std::slice;

use jstream_platform::PlatformError;
use jstream_platform::media::MediaTime;
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVReturnSuccess,
};

use crate::annexb::media_time_from_cm;
use crate::fourcc::os_status;
use crate::timebase;

/// A presentation time on the host clock (the Mach time line) as a media-clock time.
/// `what` names the frame in the error, such as "a camera frame".
pub(crate) fn presentation_time(time: CMTime, what: &str) -> Result<MediaTime, PlatformError> {
    if !time.flags.contains(CMTimeFlags::Valid) {
        return Err(PlatformError::Os {
            call: "CMSampleBufferGetPresentationTimeStamp",
            code: None,
            message: format!("{what} had no valid presentation time"),
        });
    }
    media_time_from_cm(time.value, time.timescale, timebase())
}

/// A pixel buffer locked for reading, unlocked when dropped.
pub(crate) struct ReadLocked<'a> {
    buffer: &'a CVPixelBuffer,
    /// The frame, for errors, such as "a camera frame".
    what: &'static str,
}

impl<'a> ReadLocked<'a> {
    /// Locks `buffer` read-only. `what` names the frame in errors.
    pub(crate) fn lock(
        buffer: &'a CVPixelBuffer,
        what: &'static str,
    ) -> Result<Self, PlatformError> {
        // SAFETY: the buffer is live; a read-only lock maps it for reading.
        let status =
            unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
        if status != kCVReturnSuccess {
            return Err(os_status(
                "CVPixelBufferLockBaseAddress",
                status,
                &format!("locking {what}"),
            ));
        }
        Ok(Self { buffer, what })
    }

    /// A packed buffer's `height` rows of `stride` bytes, which the caller has checked
    /// against its format.
    pub(crate) fn bytes(&self, stride: usize, height: usize) -> Result<&[u8], PlatformError> {
        let fail = |call: &'static str, message: String| PlatformError::Os {
            call,
            code: None,
            message,
        };
        let base = NonNull::new(CVPixelBufferGetBaseAddress(self.buffer).cast::<u8>()).ok_or_else(
            || {
                fail(
                    "CVPixelBufferGetBaseAddress",
                    format!("{} had no pixels", self.what),
                )
            },
        )?;
        let len = stride.checked_mul(height).ok_or_else(|| {
            fail(
                "CVPixelBufferGetBytesPerRow",
                format!("{}'s size overflows", self.what),
            )
        })?;
        // SAFETY: while the buffer is locked, Core Video's base address of a packed
        // buffer points at `height` rows of `stride` bytes it owns. The borrow is tied to
        // `&self`, so it ends before the unlock, and nothing writes to a buffer locked
        // read-only.
        Ok(unsafe { slice::from_raw_parts(base.as_ptr(), len) })
    }

    /// Plane `index`'s first `rows` rows, and its stride.
    pub(crate) fn plane(&self, index: usize, rows: usize) -> Result<(&[u8], usize), PlatformError> {
        let short = || PlatformError::Unsupported {
            what: format!("plane {index} of {}, shorter than its format", self.what),
        };
        if CVPixelBufferGetHeightOfPlane(self.buffer, index) < rows {
            return Err(short());
        }
        let base =
            NonNull::new(CVPixelBufferGetBaseAddressOfPlane(self.buffer, index).cast::<u8>())
                .ok_or_else(short)?;
        let stride = CVPixelBufferGetBytesPerRowOfPlane(self.buffer, index);
        let len = stride.checked_mul(rows).ok_or_else(short)?;
        // SAFETY: while the buffer is locked, Core Video's base address for the plane
        // points at its rows (at least `rows`, checked above) of `stride` bytes, which it
        // owns. The borrow is tied to `&self`, so it ends before the unlock, and nothing
        // writes to a buffer locked read-only.
        Ok((unsafe { slice::from_raw_parts(base.as_ptr(), len) }, stride))
    }
}

impl Drop for ReadLocked<'_> {
    fn drop(&mut self) {
        // SAFETY: this buffer was locked with the same (read-only) flags in `lock`.
        let _ = unsafe {
            CVPixelBufferUnlockBaseAddress(self.buffer, CVPixelBufferLockFlags::ReadOnly)
        };
    }
}
