//! GPU interop on Linux (`jstream-platform` README › GPU frames, ADR-0060 §3, ADR-0004):
//! the engine's wgpu device on Vulkan, with DMA-BUF import where the driver has it, so
//! the ScreenCast portal's PipeWire frames reach the compositor without a CPU copy.
//!
//! - [`LinuxGpu::open_device`] opens a Vulkan device on a hardware adapter, asking for
//!   wgpu's `VULKAN_EXTERNAL_MEMORY_DMA_BUF` (`VK_EXT_external_memory_dma_buf` and
//!   `VK_EXT_image_drm_format_modifier`) where the adapter has it, and records which
//!   DRM format modifiers it can import and sample for each format PipeWire offers
//!   ([`dmabuf::importable_modifiers`]).
//! - Capture reaches the context through `PortalCapture::on_context`. Its PipeWire
//!   stream offers those modifiers, imports each DMA-BUF it gets as a Vulkan image
//!   ([`dmabuf::import`]), and copies it on the context's queue into a texture of its
//!   own ([`blit::Ring`]), which is the frame render samples. The PipeWire buffer goes
//!   back to the compositor once that copy is done, so the compositor never writes a
//!   buffer the GPU still reads, and render never holds one.
//! - Everything runs on the context's one queue, so a frame needs no fence: wgpu orders
//!   the copy before every later submit that samples the texture. [`QueueOrder`] is the
//!   frames' [`FrameSync`], whose `stage` only checks the queue.
//! - The encoders take CPU frames (VA-API's upload, OpenH264), so there are no encode
//!   targets yet: [`LinuxGpu::encode_target`] reports `Unsupported`.

pub(crate) mod blit;
pub(crate) mod dmabuf;

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use jstream_platform::media::FrameSize;
use jstream_platform::{AdapterId, FrameSync, GpuContext, GpuFrame, GpuInterop, PlatformError};
use wgpu::hal::api::Vulkan;

use crate::spa::SpaVideoFormat;

/// GPU interop through Vulkan (wgpu), with DMA-BUF import for capture.
#[derive(Debug, Default)]
pub struct LinuxGpu {
    /// Whether a software adapter (lavapipe) may be opened. Only tests set it: the
    /// engine's own fallback is a software device without the platform's interop.
    software: bool,
}

impl LinuxGpu {
    /// The backend. Opening nothing yet, it can't fail.
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend that also opens a software adapter, for tests on machines without a
    /// GPU.
    #[cfg(test)]
    pub(crate) fn with_software() -> Self {
        Self { software: true }
    }
}

/// The platform's state for one [`GpuContext`].
pub(crate) struct Shared {
    /// For each format PipeWire may send, the modifiers this device can import as a
    /// single-plane DMA-BUF and sample. Empty without DMA-BUF import.
    pub(crate) modifiers: Vec<(SpaVideoFormat, Vec<u64>)>,
    /// The device's raw queue, to recognise it in [`FrameSync::stage`]. (Comparing
    /// `wgpu::Queue`s doesn't work: wgpu-core ids restart in every `Instance`.)
    raw_queue: u64,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("modifiers", &self.modifiers)
            .finish_non_exhaustive()
    }
}

impl Shared {
    /// The state of `context`, which must have been opened by [`LinuxGpu`].
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context another backend opened.
    pub(crate) fn of(context: &GpuContext, what: &'static str) -> Result<Arc<Self>, PlatformError> {
        context
            .platform::<Arc<Self>>()
            .cloned()
            .ok_or(PlatformError::InvalidState {
                what,
                state: "the context was not opened by the Linux GPU interop",
            })
    }

    /// Whether this device imports DMA-BUFs in any format PipeWire may send.
    pub(crate) fn imports_dma_bufs(&self) -> bool {
        self.modifiers.iter().any(|(_, m)| !m.is_empty())
    }
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

fn not_ready(what: &str) -> PlatformError {
    PlatformError::Os {
        call: "wgpu",
        code: None,
        message: format!("{what} didn't complete at once on the native backend"),
    }
}

/// The adapter's raw `VkQueue` handle.
fn raw_queue(queue: &wgpu::Queue) -> Option<u64> {
    use ash::vk::Handle;
    // SAFETY: only the queue's handle is read, to compare queues; it is never used to
    // submit.
    let hal = unsafe { queue.as_hal::<Vulkan>() }?;
    Some(hal.as_raw().as_raw())
}

/// Orders adapters for the media device: those that import DMA-BUFs first, then
/// integrated before discrete GPUs (on a laptop with both, the desktop usually runs on
/// the integrated one, and its frames import there without crossing adapters), then
/// the rest. Software adapters only when `software` is set.
fn rank(device_type: wgpu::DeviceType, dma_buf: bool, software: bool) -> Option<u8> {
    let kind = match device_type {
        wgpu::DeviceType::IntegratedGpu => 0,
        wgpu::DeviceType::DiscreteGpu => 1,
        wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => 2,
        wgpu::DeviceType::Cpu if software => 3,
        wgpu::DeviceType::Cpu => return None,
    };
    Some(if dma_buf { kind } else { kind + 4 })
}

impl GpuInterop for LinuxGpu {
    /// Opens a Vulkan device on the media adapter: the first hardware adapter, those
    /// that import DMA-BUFs first, then integrated before discrete GPUs, with DMA-BUF
    /// import when it has it. The desktop doesn't say which GPU
    /// it composites on, so this is a guess; a frame that doesn't import ends the GPU
    /// path for its capture, which then copies on the CPU.
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapters = ready(instance.enumerate_adapters(wgpu::Backends::VULKAN))
            .ok_or_else(|| not_ready("enumerate_adapters"))?;
        let dma_buf = |a: &wgpu::Adapter| {
            a.features()
                .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
        };
        let adapter = adapters
            .into_iter()
            .filter_map(|a| {
                rank(a.get_info().device_type, dma_buf(&a), self.software).map(|r| (r, a))
            })
            .min_by_key(|(r, _)| *r)
            .map(|(_, a)| a)
            .ok_or(PlatformError::Unsupported {
                what: "GPU interop: no Vulkan hardware adapter".into(),
            })?;
        let imports = dma_buf(&adapter);
        let (device, queue) = ready(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jstream"),
            required_features: if imports {
                wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF
            } else {
                wgpu::Features::empty()
            },
            ..Default::default()
        }))
        .ok_or_else(|| not_ready("request_device"))?
        .map_err(|e| PlatformError::Os {
            call: "wgpu::Adapter::request_device",
            code: None,
            message: e.to_string(),
        })?;
        let modifiers = if imports {
            dmabuf::importable_modifiers(&adapter)
        } else {
            Vec::new()
        };
        let raw_queue = raw_queue(&queue).ok_or(PlatformError::Os {
            call: "wgpu::Queue::as_hal",
            code: None,
            message: "the queue is not Vulkan".into(),
        })?;
        let id = AdapterId {
            luid: None,
            vendor: adapter.get_info().vendor,
        };
        let shared = Arc::new(Shared {
            modifiers,
            raw_queue,
            device: device.clone(),
            queue: queue.clone(),
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
        Shared::of(context, "encode_target")?;
        Err(PlatformError::Unsupported {
            what: format!(
                "a {}x{} GPU encode target: Linux's encoders take CPU frames",
                size.width(),
                size.height()
            ),
        })
    }
}

/// The [`FrameSync`] of a captured frame on the context's device: the frame was
/// written by a submit on the context's own queue, so wgpu already orders it before
/// every later use, and staging only checks that the queue is that one.
#[derive(Debug)]
pub(crate) struct QueueOrder {
    shared: Arc<Shared>,
}

impl QueueOrder {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl FrameSync for QueueOrder {
    fn stage(&self, queue: &wgpu::Queue) -> Result<(), PlatformError> {
        if raw_queue(queue) == Some(self.shared.raw_queue) {
            Ok(())
        } else {
            Err(PlatformError::InvalidState {
                what: "FrameSync::stage",
                state: "the queue belongs to another device than the frame's context",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::FrameSize;
    use jstream_platform::{GpuInterop, PlatformError};

    use super::{LinuxGpu, Shared, rank};

    #[test]
    fn adapters_rank_by_import_then_kind() {
        use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu, Other};
        let order: Vec<Option<u8>> = [
            (IntegratedGpu, true),
            (DiscreteGpu, true),
            (Other, true),
            (IntegratedGpu, false),
            (DiscreteGpu, false),
        ]
        .into_iter()
        .map(|(kind, dma)| rank(kind, dma, false))
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        assert_eq!(rank(Cpu, true, false), None, "no software adapter");
        assert!(rank(Cpu, true, true) > rank(Other, true, true));
    }

    /// On any machine: a Vulkan device opens (a software one here, if that is all there
    /// is) or the backend says there is none; either way it never panics. The context
    /// is recognised as ours, and has no encode targets.
    #[test]
    fn a_context_opens_or_is_unsupported() {
        let gpu = LinuxGpu::with_software();
        let context = match gpu.open_device() {
            Ok(context) => context,
            Err(PlatformError::Unsupported { .. }) => return,
            Err(e) => panic!("{e}"),
        };
        let shared = Shared::of(&context, "test").expect("our context");
        let size = FrameSize::new(64, 64).expect("size");
        assert!(matches!(
            gpu.encode_target(&context, size),
            Err(PlatformError::Unsupported { .. })
        ));
        // Whatever modifiers there are, each format is listed once.
        let mut formats: Vec<_> = shared.modifiers.iter().map(|(f, _)| *f).collect();
        formats.dedup();
        assert_eq!(formats.len(), shared.modifiers.len());
    }
}
