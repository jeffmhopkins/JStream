//! Capture's side of the GPU path (ARCH › GPU pipeline, steps 1 and 2): a ring of BGRA
//! textures shared between D3D11 and D3D12, which WGC frames are copied into, and the
//! fence that says when a copy is done.
//!
//! Each capture stream has its own fence, which only its frame handler signals, so its
//! values only increase. A frame's [`FrameSync::stage`] waits on that fence for the
//! frame's copy, and signals the context's fence when the GPU is done with it. A slot is
//! written again only once that signal has passed, or never staged at all.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{self, AtomicU64, Ordering};

use jstream_platform::media::{ColorInfo, ColorRange, ColorSpace, MediaTime, PixelFormat};
use jstream_platform::media::{FrameSize, VideoFrameInfo};
use jstream_platform::{FrameSync, GpuContext, GpuFrame, PlatformError};
use wgpu::hal::api::Dx12;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM_SRGB;
use windows::core::Interface;

use super::Shared;
use super::d3d::{SharedFence, SharedTexture};
use crate::capture::ring::{Ring, Slot};
use crate::com::OsResult;

/// Slots per capture: one for the frame render is reading, one waiting in the mailbox,
/// one being copied into, and one spare for a frame render holds a tick longer.
const SLOTS: usize = 4;

/// The context's GPU state, for a capture backend to deliver GPU frames with.
#[derive(Clone)]
pub(crate) struct CaptureGpu {
    shared: Arc<Shared>,
}

impl CaptureGpu {
    /// The state of `context`, which must have been opened by `WindowsGpu`.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context another backend opened.
    pub(crate) fn of(context: &GpuContext) -> Result<Self, PlatformError> {
        let shared = context
            .platform::<Arc<Shared>>()
            .ok_or(PlatformError::InvalidState {
                what: "WgcCapture::on_gpu",
                state: "the context was not opened by the Windows GPU interop",
            })?;
        Ok(Self {
            shared: Arc::clone(shared),
        })
    }

    /// The context's D3D11 device, for the WGC frame pool, so frames arrive on the
    /// adapter the context is on.
    pub(crate) fn d3d11(&self) -> Result<ID3D11Device, PlatformError> {
        self.shared.d3d11.device.cast().os("ID3D11Device")
    }

    /// A ring for one capture stream, with its own fence.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the driver refuses the fence.
    pub(crate) fn ring(&self) -> Result<CaptureRing, PlatformError> {
        let fence = SharedFence::new(&self.shared.d3d11.device, &self.shared.d3d12)?;
        Ok(CaptureRing {
            slots: Ring::new(SLOTS),
            fence: Arc::new(CaptureFence {
                fence,
                last: AtomicU64::new(0),
            }),
            shared: Arc::clone(&self.shared),
        })
    }
}

impl fmt::Debug for CaptureGpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptureGpu").finish_non_exhaustive()
    }
}

/// A capture stream's fence: D3D11 signals it after each copy.
struct CaptureFence {
    fence: SharedFence,
    /// The last value signalled. Only the stream's frame handler (one at a time, under
    /// its lock) signals, so the values increase.
    last: AtomicU64,
}

/// One slot: a shared texture, its wgpu wrapper and its synchronisation hold.
struct CaptureSlot {
    // The hold drops before the wgpu texture, as in `GpuFrame`.
    sync: Arc<CaptureSync>,
    texture: wgpu::Texture,
    size: (u32, u32),
}

impl Slot for CaptureSlot {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Nothing but the ring holds it, and the GPU has passed its last staged use.
    fn is_free(&self) -> bool {
        if Arc::strong_count(&self.sync) != 1 {
            return false;
        }
        // Pairs with the release in the last holder's drop, so its stage is seen.
        atomic::fence(Ordering::Acquire);
        let released = self.sync.released.load(Ordering::Acquire);
        // SAFETY: plain COM call on the live shared fence. A removed device reports
        // u64::MAX, which frees every slot; the next copy then fails and ends the capture.
        released == 0 || unsafe { self.sync.shared.fence.d3d12.GetCompletedValue() } >= released
    }
}

/// The ring of one capture stream. Used by its frame handler only.
pub(crate) struct CaptureRing {
    // Raw objects drop before `shared`, which holds the wgpu device clone.
    slots: Ring<CaptureSlot>,
    fence: Arc<CaptureFence>,
    shared: Arc<Shared>,
}

// SAFETY: the slots' D3D11 and D3D12 objects and the fences are free-threaded resources
// (no context state). The immediate context is used through `Shared`, whose D3D11 device
// is multithread-protected. The ring moves between WinRT thread-pool threads, and its
// stream's lock lets one of them at a time use it.
unsafe impl Send for CaptureRing {}

impl CaptureRing {
    /// Copies the top-left `size` of `source`, a BGRA texture on the context's D3D11
    /// device, into a free slot, and returns the slot as a frame at `pts`. `Ok(None)`
    /// when every slot is held (the frame is dropped).
    ///
    /// The copy is queued on D3D11, which signals the stream's fence after it; staging
    /// the frame waits for that signal. Never waits for the GPU.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the driver refuses a texture or the signal.
    pub(crate) fn copy(
        &mut self,
        source: &ID3D11Texture2D,
        size: FrameSize,
        pts: MediaTime,
    ) -> Result<Option<GpuFrame>, PlatformError> {
        let (w, h) = (size.width(), size.height());
        let Some(slot) = self
            .slots
            .acquire((w, h), || allocate(&self.shared, &self.fence, (w, h)))?
        else {
            return Ok(None);
        };
        let context = &self.shared.d3d11.context;
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: w,
            bottom: h,
            back: 1,
        };
        let ready = self.fence.last.load(Ordering::Relaxed) + 1;
        // SAFETY: both textures are live and on the context's D3D11 device, their formats
        // are in one typeless group (BGRA UNORM into BGRA UNORM_SRGB), and the box lies
        // within both (the caller clips `size` to the source; the slot has that size).
        // No GPU work uses the slot: it is free.
        unsafe {
            context.CopySubresourceRegion(
                &slot.sync.texture.d3d11,
                0,
                0,
                0,
                0,
                source,
                0,
                Some(&region),
            );
        }
        // SAFETY: plain COM calls on live objects; `ready` is above every value this
        // fence was signalled with.
        unsafe { context.Signal(&self.fence.fence.d3d11, ready) }
            .os("ID3D11DeviceContext4::Signal")?;
        // SAFETY: plain COM call. Submits the copy and the signal now, as S1 did, so
        // render's wait isn't held up by D3D11 batching.
        unsafe { context.Flush() };
        self.fence.last.store(ready, Ordering::Relaxed);
        slot.sync.ready.store(ready, Ordering::Release);
        let info = VideoFrameInfo {
            size,
            format: PixelFormat::Bgra8,
            // Desktop pixels: sRGB, which has BT.709's primaries, in full range.
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            pts,
        };
        Ok(Some(GpuFrame::new(
            slot.texture.clone(),
            info,
            Arc::clone(&slot.sync) as Arc<dyn FrameSync>,
        )))
    }
}

/// A new slot of `size`.
fn allocate(
    shared: &Arc<Shared>,
    fence: &Arc<CaptureFence>,
    size: (u32, u32),
) -> Result<CaptureSlot, PlatformError> {
    let texture = SharedTexture::new(
        &shared.d3d11.device,
        &shared.d3d12,
        size,
        DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
    )?;
    let wgpu_texture = shared.import(
        &texture,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        size,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        "jstream capture (BGRA, shared)",
    );
    Ok(CaptureSlot {
        sync: Arc::new(CaptureSync {
            texture,
            fence: Arc::clone(fence),
            shared: Arc::clone(shared),
            ready: AtomicU64::new(0),
            released: AtomicU64::new(0),
        }),
        texture: wgpu_texture,
        size,
    })
}

/// The hold on one capture slot. Staging waits on the stream's fence for `ready` and
/// signals the context's fence with a new `released`.
struct CaptureSync {
    // Raw objects drop before `shared`, which holds the wgpu device clone.
    texture: SharedTexture,
    fence: Arc<CaptureFence>,
    shared: Arc<Shared>,
    /// The stream fence's value after the copy of the slot's current frame.
    ready: AtomicU64,
    /// The context fence's value after the GPU's last staged use, or 0 if none.
    released: AtomicU64,
}

// SAFETY: the texture's D3D11 and D3D12 objects and the fence are free-threaded resources
// (no context state); the rest is `Shared` (Send + Sync) and atomics.
unsafe impl Send for CaptureSync {}
// SAFETY: as above.
unsafe impl Sync for CaptureSync {}

impl fmt::Debug for CaptureSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptureSync")
            .field("ready", &self.ready.load(Ordering::Relaxed))
            .field("released", &self.released.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl FrameSync for CaptureSync {
    fn stage(&self, queue: &wgpu::Queue) -> Result<(), PlatformError> {
        // SAFETY: as in `SlotSync::stage`: the hal queue only compares its raw queue's
        // identity and stages fence operations that wgpu applies around its next submit.
        let hal_queue = unsafe { queue.as_hal::<Dx12>() }.ok_or(PlatformError::InvalidState {
            what: "FrameSync::stage",
            state: "the queue is not a DX12 queue",
        })?;
        if hal_queue.as_raw().as_raw() != self.shared.raw_queue.as_raw() {
            return Err(PlatformError::InvalidState {
                what: "FrameSync::stage",
                state: "the queue belongs to another device than the frame's context",
            });
        }
        let ready = self.ready.load(Ordering::Acquire);
        let signal = self.shared.next_value();
        hal_queue.add_wait_fence(self.fence.fence.d3d12.clone(), ready);
        hal_queue.add_signal_fence(self.shared.fence.d3d12.clone(), signal);
        // A frame staged on several submits is released by the last one: `fetch_max`,
        // since values only increase.
        self.released.fetch_max(signal, Ordering::Release);
        Ok(())
    }
}
