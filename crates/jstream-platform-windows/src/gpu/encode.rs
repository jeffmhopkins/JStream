//! The encoders' side of the GPU path (ARCH › GPU pipeline, step 5): reading an encode
//! target on D3D11, between the GPU's write and its next one.
//!
//! An encoder copies each target into a texture of its own (one GPU copy) and encodes
//! that, so the target is released as soon as the copy is queued, however long the
//! encoder holds its input. The copy waits on the context's fence for the target's
//! last write, and is followed by a signal of the release fence, which the GPU's next
//! write of the target waits for (the `gpu` module's fence values). Neither waits on
//! the CPU.

use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use jstream_platform::{GpuContext, GpuFrame, PlatformError};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device5, ID3D11Texture2D};

use super::{Shared, SlotSync};
use crate::com::OsResult;

/// The context's GPU state, for an encoder to read its encode targets with.
#[derive(Clone)]
pub(crate) struct EncodeGpu {
    shared: Arc<Shared>,
    luid: Option<u64>,
}

impl EncodeGpu {
    /// The state of `context`, which must have been opened by `WindowsGpu`.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context another backend opened.
    pub(crate) fn of(context: &GpuContext) -> Result<Self, PlatformError> {
        let shared = context
            .platform::<Arc<Shared>>()
            .ok_or(PlatformError::InvalidState {
                what: "MfCodecs::on_context",
                state: "the context was not opened by the Windows GPU interop",
            })?;
        Ok(Self {
            shared: Arc::clone(shared),
            luid: context.adapter_id().luid,
        })
    }

    /// The context's D3D11 device, for Media Foundation's DXGI device manager: created
    /// multithread-protected and with video support (`d3d::create_d3d11`).
    pub(crate) fn device(&self) -> &ID3D11Device5 {
        &self.shared.d3d11.device
    }

    /// The adapter's LUID, to pick encoders on the same adapter.
    pub(crate) fn luid(&self) -> Option<u64> {
        self.luid
    }

    /// Queues a copy of `frame`, an encode target of this context, into the top-left of
    /// `subresource` of `dest`, a texture of the same format and at least the same size
    /// on the context's D3D11 device (an allocator may align its sizes up), and
    /// releases the target to the GPU after it. Never waits for the GPU.
    ///
    /// The caller drops `frame` after this, so the producer can write the target again
    /// once it holds the only clone; that write waits for the copy.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a frame that isn't an encode target of this
    /// context, or of another size, and [`PlatformError::Os`] when the driver refuses the
    /// wait or the signal.
    pub(crate) fn copy(
        &self,
        frame: &GpuFrame,
        dest: &ID3D11Texture2D,
        subresource: u32,
    ) -> Result<(), PlatformError> {
        let slot = frame
            .sync()
            .platform()
            .and_then(|sync| sync.downcast_ref::<SlotSync>())
            .filter(|slot| Arc::ptr_eq(&slot.shared, &self.shared))
            .ok_or_else(|| PlatformError::Unsupported {
                what: "a GPU frame that is not an encode target of the encoder's context".into(),
            })?;
        let mut source = Default::default();
        // SAFETY: plain COM call on a live texture; it fills the descriptor.
        unsafe { slot.texture.d3d11.GetDesc(&mut source) };
        let mut target = Default::default();
        // SAFETY: as above.
        unsafe { dest.GetDesc(&mut target) };
        if source.Format != target.Format
            || source.Width > target.Width
            || source.Height > target.Height
        {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "copying a {}x{} {:?} encode target into a {}x{} {:?} texture",
                    source.Width,
                    source.Height,
                    source.Format,
                    target.Width,
                    target.Height,
                    target.Format
                ),
            });
        }
        let written = slot.written.load(Ordering::Acquire);
        let context = &self.shared.d3d11.context;
        // Held from the wait to the signal, so the release values increase in the order
        // they are signalled.
        let mut last = self
            .shared
            .last_release
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let released = *last + 1;
        // SAFETY: plain COM calls on live objects on the context's D3D11 device. The
        // source has one mip and the destination's format, and fits inside it (checked
        // above), so the copy with no box stays within both; `subresource` is the
        // destination's own (the caller's contract). The wait is
        // queued on the GPU; it does not block this thread.
        unsafe {
            context
                .Wait(&self.shared.fence.d3d11, written)
                .os("ID3D11DeviceContext4::Wait")?;
            context.CopySubresourceRegion(dest, subresource, 0, 0, 0, &slot.texture.d3d11, 0, None);
            context
                .Signal(&self.shared.release.d3d11, released)
                .os("ID3D11DeviceContext4::Signal")?;
            // Submits now, as capture does, so the GPU's next write isn't held up by
            // D3D11 batching.
            context.Flush();
        }
        *last = released;
        drop(last);
        slot.released.fetch_max(released, Ordering::Release);
        Ok(())
    }
}

impl fmt::Debug for EncodeGpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodeGpu")
            .field("luid", &self.luid)
            .finish_non_exhaustive()
    }
}
