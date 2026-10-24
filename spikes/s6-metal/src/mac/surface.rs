//! IOSurface-backed pixel buffers, as ScreenCaptureKit and VideoToolbox hand them over,
//! and the CPU read of a frame's probe blocks.

use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetIOSurface,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey, kCVReturnSuccess,
};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};

use crate::err::{Context, Result, bail};

/// `kCVPixelFormatType_32BGRA`.
pub const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` (NV12, video range).
pub const NV12: u32 = u32::from_be_bytes(*b"420v");

/// A pixel buffer and the IOSurface behind it.
pub struct Surface {
    pub buffer: CFRetained<CVPixelBuffer>,
    pub surface: CFRetained<IOSurfaceRef>,
}

// SAFETY: a CVPixelBuffer and its IOSurface are reference counted thread-safely and have
// no thread affinity; the spike only reads their contents with IOSurfaceLock held, or
// hands them to Metal and VideoToolbox, which take their own references.
unsafe impl Send for Surface {}
// SAFETY: as above; nothing here mutates them through `&self`.
unsafe impl Sync for Surface {}

impl Surface {
    /// A `width` x `height` pixel buffer of `format`, backed by an IOSurface that Metal can
    /// wrap as a texture.
    pub fn new(width: u32, height: u32, format: u32) -> Result<Self> {
        // SAFETY: Core Video constants, set before any code runs and never written.
        let (iosurface_key, metal_key) = unsafe {
            (
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            )
        };
        let empty = CFDictionary::<CFString, CFType>::empty();
        let yes = CFBoolean::new(true);
        let keys: [&CFString; 2] = [iosurface_key, metal_key];
        let values: [&CFType; 2] = [&empty, yes];
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: the attributes map the documented keys to a dictionary and a boolean;
        // `raw` is a valid out pointer for the +1 buffer.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width as usize,
                height as usize,
                format,
                Some(attributes.as_ref()),
                NonNull::from(&mut raw),
            )
        };
        if status != kCVReturnSuccess {
            bail!("CVPixelBufferCreate({width}x{height}) failed: {status}");
        }
        let raw = NonNull::new(raw).context("CVPixelBufferCreate returned no buffer")?;
        // SAFETY: a Create function returned it, so this takes over its +1 reference.
        let buffer = unsafe { CFRetained::from_raw(raw) };
        let surface = CVPixelBufferGetIOSurface(Some(&buffer))
            .context("the pixel buffer has no IOSurface")?;
        Ok(Self { buffer, surface })
    }

    /// The bytes at (`x`, `y`) of `plane`, `n` of them, read on the CPU with the IOSurface
    /// locked read-only.
    pub fn read(&self, plane: usize, x: usize, y: usize, out: &mut [u8]) -> Result<()> {
        let options = IOSurfaceLockOptions::ReadOnly;
        // SAFETY: the surface is live; a null seed pointer asks for no seed.
        let status = unsafe { self.surface.lock(options, ptr::null_mut()) };
        if status != 0 {
            bail!("IOSurfaceLock failed: {status}");
        }
        let width = self.surface.width_of_plane(plane);
        let height = self.surface.height_of_plane(plane);
        let stride = self.surface.bytes_per_row_of_plane(plane);
        let base = self.surface.base_address_of_plane(plane);
        let result = if y >= height || x + out.len() > stride || x >= width {
            Err(crate::err::Error::msg(format!(
                "probe at ({x}, {y}) is outside plane {plane} ({width}x{height})"
            )))
        } else {
            // SAFETY: the surface is locked, so `base` addresses `stride * height` readable
            // bytes of this plane, and the range is inside row `y`.
            let row = unsafe {
                std::slice::from_raw_parts(base.as_ptr().cast::<u8>().add(y * stride), stride)
            };
            out.copy_from_slice(&row[x..x + out.len()]);
            Ok(())
        };
        // SAFETY: unlocks the lock taken above, with the same options.
        let _ = unsafe { self.surface.unlock(options, ptr::null_mut()) };
        result
    }
}
