//! The GPU interop on Metal (ADR-0063 §8, ARCH › GPU pipeline), ported from spike S6.
//!
//! # Behaviour
//!
//! **[`MetalGpu`]** (`GpuInterop`):
//! - **`open_device`** opens the engine's wgpu device on Metal: the high-performance
//!   hardware adapter (every Apple Silicon Mac has one GPU), with the timestamp queries the
//!   compositor times effect passes with where the adapter has them. Beside it, it makes
//!   one `MTLSharedEvent`, the context's **release event**. The `AdapterId` carries the
//!   Metal device's IORegistry ID, the same in every process, and the PCI vendor ID wgpu
//!   reports.
//! - **`encode_target`** allocates an NV12 (`420v`, video range) pixel buffer backed by an
//!   IOSurface, and wraps its two planes as the render targets of a planar `GpuFrame`
//!   (`GpuFrame::planar`): the Y plane as `R8Unorm` and the CbCr plane as `Rg8Unorm`, half
//!   the size, because wgpu has no `NV12` textures on Metal. VideoToolbox takes the pixel
//!   buffer itself, with no copy ([`EncodeInput`]).
//!
//! **GPU frames from pixel buffers** ([`SurfaceGpu`], for capture): a BGRA pixel buffer
//! that is backed by an IOSurface is wrapped as a Metal texture over that IOSurface, with
//! no copy, and adopted into the context's wgpu device through wgpu-hal
//! (`texture_from_raw`, then `create_texture_from_hal`).
//!
//! **Synchronisation.** One rule for every frame: each [`FrameSync::stage`] stages a
//! signal of the context's release event, to the context's next value, after the submit
//! that uses the frame (wgpu-hal's `add_signal_event`), and the hold remembers the last
//! such value.
//!
//! *Encode targets:* render writes a target and stages it; the encoder waits on the CPU
//! for the event to reach that value (`waitUntilSignaledValue:timeoutMS:`) before it hands
//! the pixel buffer to VideoToolbox, and keeps the frame until VideoToolbox's output
//! callback for it has run, so render never writes a target VideoToolbox still reads
//! (the engine only writes a target its ring holds alone).
//!
//! *Imported frames:* the frames ScreenCaptureKit hands over are complete when they
//! arrive, so a read needs no wait. What needs ordering is the release: the pixel buffer
//! goes back to its producer's pool when its last reference drops, and the producer then
//! writes it again, so it must not drop while a submitted read is still running. When the
//! last clone of the frame drops, its hold keeps
//! the pixel buffer until the event reaches the value of its last staged use; a frame that
//! was never staged lets go at once. The release event has one signaller, the context's
//! queue, so its values only increase. The held buffers are let go on the next stage, or
//! the next frame wrapped, once the event has passed their value.
//!
//! **Lifetimes.** The platform state for a context ([`Shared`]) holds the raw Metal
//! objects and, declared last, a clone of the wgpu device, so the device outlives them.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::pin::pin;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

use jstream_platform::media::{ColorInfo, FrameSize, MediaTime, PixelFormat, VideoFrameInfo};
use jstream_platform::{AdapterId, FrameSync, GpuContext, GpuFrame, GpuInterop, PlatformError};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType, Type};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetHeight, CVPixelBufferGetIOSurface,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVReturnSuccess,
};
use objc2_io_surface::IOSurfaceRef;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLSharedEvent, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage,
};
use wgpu::hal::api::Metal;

use super::retire::Retired;
use crate::fourcc::{CV_420V, CV_BGRA, fourcc_str, os_status};
use crate::timebase;

type SharedEvent = Retained<ProtocolObject<dyn MTLSharedEvent>>;

/// GPU interop through Metal: the engine's wgpu device, and IOSurface-backed frames
/// wrapped as its textures (module docs).
#[derive(Debug, Default)]
pub struct MetalGpu {
    _private: (),
}

impl MetalGpu {
    /// The backend. Opening nothing yet, it can't fail.
    pub fn new() -> Self {
        Self::default()
    }
}

/// A pixel buffer a frame's hold keeps alive, only to release it later.
struct PixelBuffer {
    buffer: CFRetained<CVPixelBuffer>,
}

// SAFETY: a CVPixelBuffer is reference counted thread-safely and has no thread affinity.
// Nothing here reads or writes its pixels: it is only kept alive, then released.
unsafe impl Send for PixelBuffer {}
// SAFETY: as above; nothing is reached through `&PixelBuffer`.
unsafe impl Sync for PixelBuffer {}

/// The platform's state for one [`GpuContext`].
///
/// Fields drop in order: the raw Metal objects go before `device`, a clone of the wgpu
/// device, so it outlives them (README › GPU frames, driver lifetime).
pub(crate) struct Shared {
    /// Signalled only by the context's queue, after each staged use (module docs).
    event: SharedEvent,
    /// The last value handed out for `event`.
    last_value: AtomicU64,
    /// Pixel buffers whose frames dropped while a submitted read may still run.
    retired: Mutex<Retired<PixelBuffer>>,
    /// The Metal device under the wgpu device, to wrap IOSurfaces on.
    mtl: Retained<ProtocolObject<dyn MTLDevice>>,
    /// The address of the context's raw `MTLCommandQueue`, to recognise it in
    /// `FrameSync::stage`. Only compared, never dereferenced. (Comparing `wgpu::Queue`s
    /// doesn't work: wgpu-core IDs restart in every `Instance`.)
    raw_queue: usize,
    /// Keeps the device alive until the fields above are released, and adopts the
    /// wrapped textures.
    device: wgpu::Device,
}

impl Shared {
    /// The next value of the release event. Values only increase, from 1.
    fn next_value(&self) -> u64 {
        self.last_value.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Lets go of the held pixel buffers the GPU is done with.
    fn collect(&self) {
        let done = self.event.signaledValue();
        self.retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .release(done);
    }

    /// Holds `buffer` until the release event reaches `value`.
    fn retire(&self, value: u64, buffer: PixelBuffer) {
        let done = self.event.signaledValue();
        let mut retired = self.retired.lock().unwrap_or_else(PoisonError::into_inner);
        retired.release(done);
        retired.retire(value, done, buffer);
    }
}

impl Shared {
    /// Wraps plane `plane` of `surface`, `size` in pixels, as a texture of `format` on
    /// this context's device, with no copy. The plane's layout must match `format` (the
    /// caller made or checked the pixel buffer so).
    fn wrap(
        &self,
        surface: &IOSurfaceRef,
        plane: usize,
        format: wgpu::TextureFormat,
        (width, height): (u32, u32),
        usage: wgpu::TextureUsages,
        label: &str,
    ) -> Result<wgpu::Texture, PlatformError> {
        let mtl_format = match format {
            wgpu::TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb => MTLPixelFormat::BGRA8Unorm_sRGB,
            wgpu::TextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
            wgpu::TextureFormat::Rg8Unorm => MTLPixelFormat::RG8Unorm,
            other => {
                return Err(PlatformError::Unsupported {
                    what: format!("an IOSurface plane as {other:?}"),
                });
            }
        };
        let mut mtl_usage = MTLTextureUsage::empty();
        if usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
            mtl_usage |= MTLTextureUsage::ShaderRead;
        }
        if usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
            mtl_usage |= MTLTextureUsage::RenderTarget;
        }
        // SAFETY: a class constructor with a valid format and a non-zero size (the
        // callers' sizes come from `FrameSize`s, which are never empty).
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                mtl_format,
                width as usize,
                height as usize,
                false,
            )
        };
        desc.setUsage(mtl_usage);
        let raw = self
            .mtl
            .newTextureWithDescriptor_iosurface_plane(&desc, surface, plane)
            .ok_or_else(|| {
                os(
                    "MTLDevice newTextureWithDescriptor:iosurface:plane:",
                    format!(
                        "it refused plane {plane} of an IOSurface as {width}x{height} {format:?}"
                    ),
                )
            })?;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // SAFETY: `raw` is a live 2D texture with one mip and one layer, of exactly
        // `format` and this size, made on this wgpu device's own MTLDevice.
        let hal = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                None,
            )
        };
        // SAFETY: the hal texture belongs to this device and matches the descriptor
        // below. Metal has no layouts, so the initial state only seeds wgpu's tracker;
        // `from_hal` textures count as initialised, so wgpu never clears them: an imported
        // frame holds a whole picture, and render writes a whole encode target before
        // anything reads it.
        Ok(unsafe {
            self.device.create_texture_from_hal::<Metal>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        })
    }
}

/// A `width` x `height` pixel buffer of Core Video pixel format `format`, backed by an
/// IOSurface that Metal can wrap.
pub(crate) fn surface_pixel_buffer(
    width: u32,
    height: u32,
    format: u32,
) -> Result<CFRetained<CVPixelBuffer>, PlatformError> {
    // SAFETY: Core Video constants, set when the framework loads and never written.
    let (iosurface, metal) = unsafe {
        (
            kCVPixelBufferIOSurfacePropertiesKey,
            kCVPixelBufferMetalCompatibilityKey,
        )
    };
    let empty = CFDictionary::<CFString, CFType>::empty();
    let keys: [&CFString; 2] = [iosurface, metal];
    let values: [&CFType; 2] = [&empty, CFBoolean::new(true)];
    let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    let mut raw: *mut CVPixelBuffer = ptr::null_mut();
    // SAFETY: the attributes map documented keys to a dictionary and a boolean, and `raw`
    // is a valid out pointer for the +1 buffer.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width as usize,
            height as usize,
            format,
            Some(attributes.as_ref()),
            ptr::NonNull::from(&mut raw),
        )
    };
    if status != kCVReturnSuccess {
        return Err(os_status(
            "CVPixelBufferCreate",
            status,
            &format!("allocating a {width}x{height} IOSurface pixel buffer"),
        ));
    }
    let raw = ptr::NonNull::new(raw).ok_or_else(|| os("CVPixelBufferCreate", "no buffer"))?;
    // SAFETY: a Create function returned it, so this takes over its +1 reference.
    Ok(unsafe { CFRetained::from_raw(raw) })
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("last_value", &self.last_value.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The address of a queue's raw `MTLCommandQueue`, or `None` for a queue that isn't on
/// Metal.
fn raw_queue(queue: &wgpu::Queue) -> Option<usize> {
    // SAFETY: only reads the raw queue's address, to compare it; the queue isn't used.
    let hal = unsafe { queue.as_hal::<Metal>() }?;
    Some(ptr::from_ref(hal.as_raw()).cast::<u8>() as usize)
}

/// Runs a future that is already complete (wgpu's native adapter and device requests
/// resolve on their first poll). `None` if it isn't.
fn ready<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(v) => Some(v),
        Poll::Pending => None,
    }
}

fn os(call: &'static str, message: impl Into<String>) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: message.into(),
    }
}

fn foreign(what: &'static str) -> PlatformError {
    PlatformError::InvalidState {
        what,
        state: "the context was not opened by the Metal GPU interop",
    }
}

impl GpuInterop for MetalGpu {
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = ready(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .ok_or_else(|| os("wgpu", "request_adapter didn't complete at once on Metal"))?
        .map_err(|e| PlatformError::Unsupported {
            what: format!("GPU interop: no Metal adapter ({e})"),
        })?;
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return Err(PlatformError::Unsupported {
                what: "GPU interop: the only Metal adapter is a software one".into(),
            });
        }
        let (device, queue) = ready(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jstream"),
            // Timestamp queries where the adapter has them, so the compositor can time
            // effect-pack passes on this device (#349), and the adapter's own format
            // features, so it draws models with every sample count it can (ADR-0242).
            required_features: adapter.features()
                & (wgpu::Features::TIMESTAMP_QUERY
                    | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES),
            ..Default::default()
        }))
        .ok_or_else(|| os("wgpu", "request_device didn't complete at once on Metal"))?
        .map_err(|e| os("wgpu::Adapter::request_device", e.to_string()))?;
        // SAFETY: only clones the raw MTLDevice (a retain); wgpu keeps its own reference.
        // `Shared` releases the clone before its wgpu device clone.
        let mtl = unsafe { device.as_hal::<Metal>() }
            .map(|d| d.raw_device().clone())
            .ok_or_else(|| os("wgpu::Device::as_hal", "the device is not Metal"))?;
        let raw_queue =
            raw_queue(&queue).ok_or_else(|| os("wgpu::Queue::as_hal", "the queue is not Metal"))?;
        let event = mtl
            .newSharedEvent()
            .ok_or_else(|| os("MTLDevice newSharedEvent", "it returned nothing"))?;
        let id = AdapterId {
            luid: Some(mtl.registryID()),
            vendor: adapter.get_info().vendor,
        };
        let shared = Arc::new(Shared {
            event,
            last_value: AtomicU64::new(0),
            retired: Mutex::new(Retired::default()),
            mtl,
            raw_queue,
            device: device.clone(),
        });
        Ok(GpuContext::new(
            adapter,
            device,
            queue,
            id,
            Some(Box::new(shared)),
        ))
    }

    fn encode_target(
        &self,
        context: &GpuContext,
        size: FrameSize,
    ) -> Result<GpuFrame, PlatformError> {
        let shared = context
            .platform::<Arc<Shared>>()
            .ok_or(foreign("encode_target"))?;
        let (w, h) = (size.width(), size.height());
        if w % 2 != 0 || h % 2 != 0 {
            return Err(PlatformError::Unsupported {
                what: format!("an NV12 encode target at an odd size, {w}x{h}"),
            });
        }
        let buffer = surface_pixel_buffer(w, h, CV_420V)?;
        let surface =
            CVPixelBufferGetIOSurface(Some(&buffer)).ok_or_else(|| PlatformError::Os {
                call: "CVPixelBufferGetIOSurface",
                code: None,
                message: "an encode target's pixel buffer has no IOSurface".into(),
            })?;
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        let y = shared.wrap(
            &surface,
            0,
            wgpu::TextureFormat::R8Unorm,
            (w, h),
            usage,
            "jstream encode target (Y, IOSurface)",
        )?;
        let uv = shared.wrap(
            &surface,
            1,
            wgpu::TextureFormat::Rg8Unorm,
            (w / 2, h / 2),
            usage,
            "jstream encode target (CbCr, IOSurface)",
        )?;
        let info = VideoFrameInfo {
            size,
            format: PixelFormat::Nv12,
            color: ColorInfo::default(),
            pts: MediaTime::zero(timebase()),
        };
        Ok(GpuFrame::planar(
            y,
            uv,
            info,
            Arc::new(SurfaceSync::new(&buffer, shared)),
        ))
    }
}

/// A context's GPU state, for a backend to deliver its pixel buffers as GPU frames.
#[derive(Clone, Debug)]
pub(crate) struct SurfaceGpu {
    shared: Arc<Shared>,
}

impl SurfaceGpu {
    /// The state of `context`, which must have been opened by [`MetalGpu`]. `what` names
    /// the caller in the error.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context another backend opened.
    pub(crate) fn of(context: &GpuContext, what: &'static str) -> Result<Self, PlatformError> {
        let shared = context.platform::<Arc<Shared>>().ok_or(foreign(what))?;
        Ok(Self {
            shared: Arc::clone(shared),
        })
    }

    /// Lets go of the pixel buffers whose frames dropped and whose last staged read the
    /// GPU has finished. Wrapping a frame and staging one do this too; a stream calls it
    /// when it stops, so its last frames don't wait for the context's next one.
    pub(crate) fn collect(&self) {
        self.shared.collect();
    }

    /// `buffer`, a BGRA pixel buffer backed by an IOSurface, as a frame in a texture of
    /// `format` (`Bgra8Unorm` or `Bgra8UnormSrgb`) over that IOSurface, with
    /// `TEXTURE_BINDING | COPY_SRC`. `info` describes it; its size must be the buffer's.
    /// The frame's hold keeps the buffer until the GPU is done with it (module docs).
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for another pixel format, a size that doesn't match
    /// or a buffer with no IOSurface, and [`PlatformError::Os`] when Metal refuses it.
    pub(crate) fn bgra_frame(
        &self,
        buffer: &CVPixelBuffer,
        info: VideoFrameInfo,
        format: wgpu::TextureFormat,
    ) -> Result<GpuFrame, PlatformError> {
        let shared = &self.shared;
        shared.collect();
        let pixel = CVPixelBufferGetPixelFormatType(buffer);
        if pixel != CV_BGRA {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a GPU frame in pixel format {}",
                    fourcc_str(pixel).unwrap_or_else(|| format!("{pixel:#010x}"))
                ),
            });
        }
        let (width, height) = (info.size.width(), info.size.height());
        if (
            CVPixelBufferGetWidth(buffer),
            CVPixelBufferGetHeight(buffer),
        ) != (width as usize, height as usize)
        {
            return Err(PlatformError::Unsupported {
                what: "a GPU frame whose pixel buffer isn't the frame's size".into(),
            });
        }
        if !matches!(
            format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        ) {
            return Err(PlatformError::Unsupported {
                what: format!("a BGRA GPU frame as {format:?}"),
            });
        }
        let surface =
            CVPixelBufferGetIOSurface(Some(buffer)).ok_or_else(|| PlatformError::Unsupported {
                what: "a GPU frame from a pixel buffer with no IOSurface".into(),
            })?;
        let texture = shared.wrap(
            &surface,
            0,
            format,
            (width, height),
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            "jstream frame (BGRA, IOSurface)",
        )?;
        Ok(GpuFrame::new(
            texture,
            info,
            Arc::new(SurfaceSync::new(buffer, shared)),
        ))
    }
}

/// The hold on one wrapped pixel buffer (module docs, synchronisation).
pub(crate) struct SurfaceSync {
    /// Taken only by `Drop`.
    buffer: Mutex<Option<PixelBuffer>>,
    /// The same buffer, for the encoder to hand to VideoToolbox. Released with the hold.
    input: PixelBuffer,
    shared: Arc<Shared>,
    /// The release event's value after the GPU's last staged use, or 0 if none.
    released: AtomicU64,
}

impl SurfaceSync {
    fn new(buffer: &CVPixelBuffer, shared: &Arc<Shared>) -> Self {
        Self {
            buffer: Mutex::new(Some(PixelBuffer {
                buffer: buffer.retain(),
            })),
            input: PixelBuffer {
                buffer: buffer.retain(),
            },
            shared: Arc::clone(shared),
            released: AtomicU64::new(0),
        }
    }
}

/// An encode target, as the VideoToolbox encoder reads it (module docs, encode targets).
pub(crate) struct EncodeInput<'a> {
    sync: &'a SurfaceSync,
}

impl<'a> EncodeInput<'a> {
    /// The encode target behind `frame`, if `frame` is one this interop allocated.
    pub(crate) fn of(frame: &'a GpuFrame) -> Option<Self> {
        let sync = frame.sync().platform()?.downcast_ref::<SurfaceSync>()?;
        Some(Self { sync })
    }

    /// The pixel buffer VideoToolbox reads.
    pub(crate) fn pixel_buffer(&self) -> &CVPixelBuffer {
        &self.sync.input.buffer
    }

    /// Waits up to `timeout_ms` for the GPU to finish render's last staged write. `true`
    /// once it has (or when the target was never staged).
    pub(crate) fn wait_written(&self, timeout_ms: u64) -> bool {
        let value = self.sync.released.load(Ordering::Acquire);
        value == 0
            || self.sync.shared.event.signaledValue() >= value
            || self
                .sync
                .shared
                .event
                .waitUntilSignaledValue_timeoutMS(value, timeout_ms)
    }
}

impl fmt::Debug for SurfaceSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SurfaceSync")
            .field("released", &self.released.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl FrameSync for SurfaceSync {
    fn stage(&self, queue: &wgpu::Queue) -> Result<(), PlatformError> {
        if raw_queue(queue) != Some(self.shared.raw_queue) {
            return Err(PlatformError::InvalidState {
                what: "FrameSync::stage",
                state: "the queue belongs to another device than the frame's context",
            });
        }
        self.shared.collect();
        let value = self.shared.next_value();
        // SAFETY: only stages a signal that wgpu-hal encodes after the queue's next
        // submit; the raw queue isn't touched.
        let hal = unsafe { queue.as_hal::<Metal>() }.ok_or(PlatformError::InvalidState {
            what: "FrameSync::stage",
            state: "the queue is not a Metal queue",
        })?;
        hal.add_signal_event(self.shared.event.clone(), value);
        // A frame staged on several submits is released by the last one: `fetch_max`,
        // since values only increase.
        self.released.fetch_max(value, Ordering::Release);
        Ok(())
    }

    fn platform(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

impl Drop for SurfaceSync {
    fn drop(&mut self) {
        let buffer = self
            .buffer
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let released = *self.released.get_mut();
        if let Some(buffer) = buffer
            && released > 0
        {
            self.shared.retire(released, buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jstream_platform::media::{ColorInfo, FrameSize, MediaTime, PixelFormat, VideoFrameInfo};
    use jstream_platform::{GpuInterop, PlatformError};
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferCreate, kCVPixelBufferIOSurfacePropertiesKey,
        kCVPixelBufferMetalCompatibilityKey, kCVReturnSuccess,
    };
    use objc2_metal::MTLSharedEvent;

    use super::{MetalGpu, Shared, SurfaceGpu};
    use crate::fourcc::CV_BGRA;
    use crate::timebase;

    /// An IOSurface-backed BGRA pixel buffer, as ScreenCaptureKit hands over.
    fn surface_buffer(width: u32, height: u32, format: u32) -> CFRetained<CVPixelBuffer> {
        // SAFETY: Core Video constants, set when the framework loads and never written.
        let (iosurface, metal) = unsafe {
            (
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            )
        };
        let empty = CFDictionary::<CFString, CFType>::empty();
        let keys: [&CFString; 2] = [iosurface, metal];
        let values: [&CFType; 2] = [&empty, CFBoolean::new(true)];
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: the attributes map documented keys to a dictionary and a boolean, and
        // `raw` is a valid out pointer for the +1 buffer.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width as usize,
                height as usize,
                format,
                Some(attributes.as_ref()),
                std::ptr::NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, kCVReturnSuccess);
        let raw = std::ptr::NonNull::new(raw).expect("a pixel buffer");
        // SAFETY: a Create function returned it, so this takes over its +1 reference.
        unsafe { CFRetained::from_raw(raw) }
    }

    fn info(width: u32, height: u32) -> VideoFrameInfo {
        VideoFrameInfo {
            size: FrameSize::new(width, height).expect("a size"),
            format: PixelFormat::Bgra8,
            color: ColorInfo::default(),
            pts: MediaTime::zero(timebase()),
        }
    }

    /// The Metal device opens where there is a GPU (CI's Macs have a paravirtual one),
    /// and a BGRA IOSurface reads back through it exactly as written, released once the
    /// GPU is done with it.
    #[test]
    fn an_iosurface_frame_reads_back_through_the_engine_device() {
        let context = match MetalGpu::new().open_device() {
            Ok(context) => context,
            // No GPU on this Mac: nothing to check.
            Err(PlatformError::Unsupported { .. }) => return,
            Err(e) => panic!("open_device: {e}"),
        };
        let (w, h) = (64u32, 32u32);
        let buffer = surface_buffer(w, h, CV_BGRA);
        let surface =
            objc2_core_video::CVPixelBufferGetIOSurface(Some(&buffer)).expect("an IOSurface");
        // SAFETY: the surface is live and locked for writing while it is filled; the
        // rows are inside the plane's `stride * height` bytes.
        unsafe {
            assert_eq!(
                surface.lock(
                    objc2_io_surface::IOSurfaceLockOptions::empty(),
                    std::ptr::null_mut()
                ),
                0
            );
            let stride = surface.bytes_per_row();
            let base = surface.base_address().as_ptr().cast::<u8>();
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let p = base.add(y * stride + x * 4);
                    *p = x as u8; // B
                    *p.add(1) = y as u8; // G
                    *p.add(2) = 200; // R
                    *p.add(3) = 255;
                }
            }
            surface.unlock(
                objc2_io_surface::IOSurfaceLockOptions::empty(),
                std::ptr::null_mut(),
            );
        }
        let gpu = SurfaceGpu::of(&context, "test").expect("our context");
        let frame = gpu
            .bgra_frame(&buffer, info(w, h), wgpu::TextureFormat::Bgra8Unorm)
            .expect("a GPU frame");
        let device = context.device();
        let queue = context.queue();
        let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            frame.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        frame.stage(queue).expect("staged on its own queue");
        let index = queue.submit([encoder.finish()]);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("mapped"));
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .expect("the copy finishes");
        {
            let bytes = readback
                .slice(..)
                .get_mapped_range()
                .expect("a mapped range");
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let p = y * row as usize + x * 4;
                    assert_eq!(
                        &bytes[p..p + 4],
                        &[x as u8, y as u8, 200, 255],
                        "pixel ({x}, {y})"
                    );
                }
            }
        }
        let shared = context.platform::<Arc<Shared>>().expect("our state");
        assert!(shared.event.signaledValue() >= 1, "the staged signal ran");
        drop(frame);
        assert_eq!(buffer.retain_count(), 1, "the frame let go of the buffer");
    }

    #[test]
    fn frames_refuse_other_formats_and_contexts_of_other_queues() {
        let context = match MetalGpu::new().open_device() {
            Ok(context) => context,
            // No GPU on this Mac: nothing to check.
            Err(PlatformError::Unsupported { .. }) => return,
            Err(e) => panic!("open_device: {e}"),
        };
        let gpu = SurfaceGpu::of(&context, "test").expect("our context");
        let nv12 = surface_buffer(16, 16, crate::fourcc::CV_420V);
        assert!(matches!(
            gpu.bgra_frame(&nv12, info(16, 16), wgpu::TextureFormat::Bgra8Unorm),
            Err(PlatformError::Unsupported { .. })
        ));
        let bgra = surface_buffer(16, 16, CV_BGRA);
        assert!(matches!(
            gpu.bgra_frame(&bgra, info(8, 16), wgpu::TextureFormat::Bgra8Unorm),
            Err(PlatformError::Unsupported { .. })
        ));
        let frame = gpu
            .bgra_frame(&bgra, info(16, 16), wgpu::TextureFormat::Bgra8UnormSrgb)
            .expect("a GPU frame");
        let other = MetalGpu::new().open_device().expect("a second context");
        assert!(matches!(
            frame.stage(other.queue()),
            Err(PlatformError::InvalidState { .. })
        ));
        assert!(matches!(
            MetalGpu::new().encode_target(&context, FrameSize::new(63, 64).expect("a size")),
            Err(PlatformError::Unsupported { .. })
        ));
        let target = MetalGpu::new()
            .encode_target(&context, FrameSize::new(64, 32).expect("a size"))
            .expect("an encode target");
        let (y, uv) = target.planes().expect("a planar target");
        assert_eq!(
            (y.format(), y.width(), y.height()),
            (wgpu::TextureFormat::R8Unorm, 64, 32)
        );
        assert_eq!(
            (uv.format(), uv.width(), uv.height()),
            (wgpu::TextureFormat::Rg8Unorm, 32, 16)
        );
        assert!(super::EncodeInput::of(&target).is_some());
        assert!(
            super::EncodeInput::of(&frame).is_some(),
            "any frame of ours has a buffer"
        );
    }
}
