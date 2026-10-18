//! Frames that stay on the GPU (README › GPU frames): the device the platform opens for
//! the engine, shared textures, and the fence staging around a submit.
//!
//! The types use `wgpu` (ADR-0004). Platform crates implement them with wgpu-hal, whose
//! `unsafe` calls they are allowed to make (R-UNSAFE-001); the engine and `jstream-render`
//! only call the safe methods here.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use jstream_media::{FrameSize, VideoFrameInfo};

use crate::PlatformError;

/// Which GPU a device is on, so encoders can be matched to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AdapterId {
    /// The adapter's locally unique ID (a LUID on Windows), when the OS has one.
    pub luid: Option<u64>,
    /// The PCI vendor ID, for matching encoders that don't report the LUID (AMD's
    /// encoder MFTs, per spike S1).
    pub vendor: u32,
}

/// The wgpu device the platform opened for the engine: on the **media adapter**, the
/// one that drives the captured display and that capture and encoders use, so frames
/// never cross adapters.
///
/// Fields drop in declaration order: the platform's own state (which may hold raw
/// objects taken from the device) goes first, before the queue, device and adapter, as
/// ARCH › GPU pipeline step 2 requires.
pub struct GpuContext {
    platform: Option<Box<dyn Any + Send + Sync>>,
    queue: wgpu::Queue,
    device: wgpu::Device,
    adapter: wgpu::Adapter,
    id: AdapterId,
    key: ContextKey,
}

/// Which [`GpuContext`] something was made on: equal only for the same context.
///
/// `wgpu::Device`'s own equality compares IDs that each wgpu instance numbers from
/// zero, so devices of two instances can compare equal. A texture is only valid on its
/// own device, so code that must not sample another context's texture compares these.
#[derive(Clone)]
pub struct ContextKey(Arc<()>);

impl PartialEq for ContextKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ContextKey {}

impl fmt::Debug for ContextKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContextKey({:p})", Arc::as_ptr(&self.0))
    }
}

impl GpuContext {
    /// Bundles a device the platform opened. `platform` is the platform's own state for
    /// this device, which only that platform reads back ([`platform`](Self::platform)).
    pub fn new(
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        id: AdapterId,
        platform: Option<Box<dyn Any + Send + Sync>>,
    ) -> Self {
        Self {
            platform,
            queue,
            device,
            adapter,
            id,
            key: ContextKey(Arc::new(())),
        }
    }

    /// The adapter.
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    /// The device, which `jstream-render` uses instead of creating its own.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The device's queue.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Which adapter this is.
    pub fn adapter_id(&self) -> AdapterId {
        self.id
    }

    /// This context's key, the same for nothing else.
    pub fn key(&self) -> ContextKey {
        self.key.clone()
    }

    /// The platform's state, if it is of type `T`. A platform uses this to recognise its
    /// own contexts; another platform's context gives `None`.
    pub fn platform<T: Any>(&self) -> Option<&T> {
        self.platform.as_ref()?.downcast_ref()
    }
}

impl fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuContext")
            .field("adapter", &self.adapter.get_info().name)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The platform's GPU interop: opens the engine's device and allocates the textures it
/// shares with capture and the encoders.
///
/// Shared (`Arc<dyn GpuInterop>`), so it is `Send + Sync`.
pub trait GpuInterop: Send + Sync {
    /// Opens the wgpu device on the media adapter.
    ///
    /// When this fails, the engine falls back to a device of its own (a software adapter
    /// in tests and on headless machines, ARCH principle 7) and the CPU frame paths
    /// (ARCH › GPU pipeline step 6).
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the platform has no GPU interop or no suitable
    /// adapter (for example, none with NV12 textures), and [`PlatformError::Os`] when the
    /// driver refuses.
    fn open_device(&self) -> Result<GpuContext, PlatformError>;

    /// Allocates an NV12 texture of `size` that the platform's encoders and virtual
    /// camera can read, as a frame the engine writes into. Each use is staged with
    /// [`GpuFrame::stage`]. The frame's `pts` is a placeholder; the engine sets the real
    /// one with [`GpuFrame::with_pts`] before handing it on.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context this platform didn't open,
    /// [`PlatformError::Unsupported`] for a size the adapter can't allocate, and
    /// [`PlatformError::Os`] when the driver refuses.
    fn encode_target(
        &self,
        context: &GpuContext,
        size: FrameSize,
    ) -> Result<GpuFrame, PlatformError>;
}

/// The platform's hold on a shared texture's synchronisation. Each
/// [`stage`](FrameSync::stage) is one use.
///
/// The rules for implementations (README › GPU frames):
/// - [`stage`](FrameSync::stage) stages a wait for the value that makes the texture ready
///   and a signal for the value that releases it, around the queue's next submit.
/// - A hold that is dropped without being staged releases its texture at once, since no
///   GPU work references it.
/// - An implementation that holds raw driver objects also holds a `wgpu::Device` clone,
///   dropped after them, so the driver can't be unloaded first.
pub trait FrameSync: Send + Sync + fmt::Debug {
    /// Stages this frame's wait and signal around the next submit on `queue`. Called by
    /// the engine for every frame that submit reads or writes, before submitting. Each
    /// call is one use: an encode target written every tick is staged every tick.
    ///
    /// Stage right before a submit that will happen. The staged wait and signal attach to
    /// the queue's next submit, whichever it is, so a submit that is skipped or fails
    /// validation moves them to the one after.
    ///
    /// That submit must end by transitioning the texture to `wgpu::TextureUses::PRESENT`
    /// (`CommandEncoder::transition_resources`): on D3D12 that is the `COMMON` state the
    /// platform's other APIs expect a shared texture in.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a queue of another device or backend, and
    /// [`PlatformError::Os`] when the driver refuses.
    fn stage(&self, queue: &wgpu::Queue) -> Result<(), PlatformError>;

    /// The implementation, for the platform that made the hold to recognise its own, as
    /// [`GpuContext::platform`] does for contexts: an encoder reading a frame on the
    /// platform's other API finds the texture's raw objects through it. `None`, the
    /// default, for a hold no platform reads back.
    fn platform(&self) -> Option<&dyn Any> {
        None
    }
}

/// A frame in a texture shared with the platform: what capture delivers on the GPU path,
/// and what the encoders and virtual camera take (`VideoSurface::Gpu`).
///
/// Clones share the texture and the [`FrameSync`] hold. Two frames are equal when they
/// are the same texture and hold, with the same info.
#[derive(Clone, Debug)]
pub struct GpuFrame {
    // The hold drops before the texture: the texture may keep the device (and its
    // driver) alive for the hold's raw objects.
    sync: Arc<dyn FrameSync>,
    texture: wgpu::Texture,
    info: VideoFrameInfo,
}

impl GpuFrame {
    /// A frame in `texture`, described by `info`, with the platform's `sync` hold.
    pub fn new(texture: wgpu::Texture, info: VideoFrameInfo, sync: Arc<dyn FrameSync>) -> Self {
        Self {
            sync,
            texture,
            info,
        }
    }

    /// The texture.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Size, pixel format, colour and presentation time.
    pub fn info(&self) -> &VideoFrameInfo {
        &self.info
    }

    /// The same frame with another presentation time.
    pub fn with_pts(mut self, pts: jstream_media::MediaTime) -> Self {
        self.info.pts = pts;
        self
    }

    /// The platform's synchronisation hold, for implementations that recognise their own.
    pub fn sync(&self) -> &Arc<dyn FrameSync> {
        &self.sync
    }

    /// Stages this frame's wait and signal around the next submit on `queue`
    /// ([`FrameSync::stage`]).
    ///
    /// # Errors
    /// As for [`FrameSync::stage`].
    pub fn stage(&self, queue: &wgpu::Queue) -> Result<(), PlatformError> {
        self.sync.stage(queue)
    }
}

impl PartialEq for GpuFrame {
    fn eq(&self, other: &Self) -> bool {
        self.texture == other.texture
            // The data pointers only: the same hold behind two vtables is still one hold.
            && std::ptr::addr_eq(Arc::as_ptr(&self.sync), Arc::as_ptr(&other.sync))
            && self.info == other.info
    }
}

impl Eq for GpuFrame {}
